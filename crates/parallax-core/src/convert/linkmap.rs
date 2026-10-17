//! Linker map files (`.map`) of the GNU linker (`ld -Map`) and LLVM lld.
//!
//! A map lists the memory regions of the linker script, every output section with
//! its address and size, the input sections that went into it (object file and
//! size) and the symbols defined there. It is the place to see why the firmware
//! grew, but the raw file is hard to compare: one grown function shifts the
//! address of everything after it, long section names wrap onto two lines, and
//! paths and padding fill most of the text.
//!
//! The "Memory map" view shows: memory regions with their use (output sections
//! counted by run address, and by load address for initialized data), output
//! sections sorted by address, a size per object file (in total and per output
//! section), and symbols sorted by name with address, size and object. Sizes are
//! one line per object, so a grown function shows as one changed line under its
//! object; the shifted addresses stay on the symbol lines, which line up because
//! they are sorted by name.
//!
//! GNU maps do not give symbol sizes: a symbol's size is taken as the distance to
//! the next symbol in its input section (exact with `-ffunction-sections`). Maps of
//! other linkers (IAR, TI, Green Hills …) are not detected and open as text.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::{Error, Result};

/// Symbols shown at most.
const MAX_SYMBOLS: usize = 200_000;
/// Where the detection looks for the headings of a map.
const SNIFF: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Gnu,
    Lld,
}

/// Whether a file is a GNU ld or lld map (and not, say, a JSON source map).
pub fn detect(data: &[u8]) -> Option<Flavor> {
    let head = &data[..data.len().min(SNIFF)];
    let head = String::from_utf8_lossy(head);
    let first = head.lines().find(|l| !l.trim().is_empty())?;
    let words: Vec<&str> = first.split_whitespace().collect();
    if words.starts_with(&["VMA", "LMA", "Size", "Align", "Out", "In", "Symbol"])
        || words.starts_with(&["Address", "Size", "Align", "Out", "In", "Symbol"])
    {
        return Some(Flavor::Lld);
    }
    const GNU_HEADINGS: &[&str] = &[
        "Archive member included to satisfy reference by file (symbol)",
        "Allocating common symbols",
        "Discarded input sections",
        "Memory Configuration",
        "Linker script and memory map",
        "As-needed library included to satisfy reference by file (symbol)",
        "Merging program properties",
    ];
    head.lines().any(|l| GNU_HEADINGS.contains(&l.trim_end())).then_some(Flavor::Gnu)
}

/// The "Memory map" view.
pub fn memory_map(text: &str) -> Result<String> {
    let map = match detect(text.as_bytes()) {
        Some(Flavor::Gnu) => parse_gnu(text),
        Some(Flavor::Lld) => parse_lld(text),
        None => return Err(Error::Unsupported("not a GNU ld or lld map file".into())),
    };
    if map.sections.is_empty() {
        return Err(Error::Unsupported("map file: no output sections found".into()));
    }
    Ok(map.render())
}

struct Region<'a> {
    name: &'a str,
    origin: u64,
    length: u64,
    attributes: &'a str,
}

struct Section<'a> {
    name: &'a str,
    address: u64,
    size: u64,
    load: Option<u64>,
}

struct Input<'a> {
    /// Index into the output sections.
    out: usize,
    object: &'a str,
    address: u64,
    size: u64,
}

struct Symbol<'a> {
    name: &'a str,
    address: u64,
    size: Option<u64>,
    /// Index into the inputs, when the symbol is inside one.
    input: Option<usize>,
}

#[derive(Default)]
struct Map<'a> {
    linker: &'static str,
    regions: Vec<Region<'a>>,
    sections: Vec<Section<'a>>,
    inputs: Vec<Input<'a>>,
    symbols: Vec<Symbol<'a>>,
    discarded: (usize, u64),
}

fn hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?, 16).ok()
}

fn addr(a: u64) -> String {
    format!("0x{a:08x}")
}

fn bytes(n: u64) -> String {
    match n {
        1 => "1 byte".into(),
        n if n >= 1 << 20 && n % (1 << 20) == 0 => format!("{n} bytes ({} MiB)", n >> 20),
        n if n >= 1 << 10 && n % (1 << 10) == 0 => format!("{n} bytes ({} KiB)", n >> 10),
        n => format!("{n} bytes"),
    }
}

/// The name of a symbol assignment (`_etext = .`, `PROVIDE (end = .)`), if it
/// defines one.
fn assignment(text: &str) -> Option<&str> {
    let (lhs, _) = text.split_once('=')?;
    let lhs = lhs
        .trim()
        .trim_start_matches("PROVIDE_HIDDEN")
        .trim_start_matches("PROVIDE")
        .trim()
        .trim_start_matches('(')
        .trim();
    let ok = !lhs.is_empty()
        && lhs != "."
        && lhs.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$'));
    ok.then_some(lhs)
}

fn parse_gnu(text: &str) -> Map<'_> {
    let mut map = Map { linker: "GNU ld", ..Map::default() };
    #[derive(PartialEq)]
    enum Part {
        Head,
        Discarded,
        Memory,
        Script,
    }
    let mut part = Part::Head;
    let mut lines = text.lines().peekable();
    // The output section and input section that following lines belong to.
    let mut out: Option<usize> = None;
    let mut input: Option<usize> = None;
    let mut pending_input: Option<&str> = None;
    while let Some(line) = lines.next() {
        match line.trim_end() {
            "Discarded input sections" => {
                part = Part::Discarded;
                continue;
            }
            "Memory Configuration" => {
                part = Part::Memory;
                continue;
            }
            "Linker script and memory map" => {
                part = Part::Script;
                continue;
            }
            _ => {}
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match part {
            Part::Head => {}
            Part::Discarded => {
                // ` .name 0x0 0x20 file`, the name possibly on its own line.
                if let [.., a, size, _obj] = words.as_slice()
                    && hex(a).is_some()
                    && let Some(size) = hex(size)
                {
                    map.discarded.0 += 1;
                    map.discarded.1 += size;
                }
            }
            Part::Memory => {
                if let [name, origin, length, rest @ ..] = words.as_slice()
                    && *name != "*default*"
                    && let (Some(origin), Some(length)) = (hex(origin), hex(length))
                {
                    map.regions.push(Region { name, origin, length, attributes: rest.first().copied().unwrap_or("") });
                }
            }
            Part::Script => {
                if words.is_empty() {
                    continue;
                }
                let indent = line.len() - line.trim_start().len();
                if indent == 0 {
                    // An output section, or LOAD / OUTPUT / START GROUP lines.
                    input = None;
                    let name = words[0];
                    if matches!(
                        name,
                        "LOAD" | "START" | "END" | "OUTPUT" | "OUTPUT_FORMAT" | "TARGET" | "INPUT" | "GROUP"
                    ) || name.contains('(')
                        || name == "/DISCARD/"
                    {
                        out = None;
                        continue;
                    }
                    let mut rest: Vec<&str> = words[1..].to_vec();
                    if rest.is_empty()
                        && let Some(next) = lines.peek()
                    {
                        let next_words: Vec<&str> = next.split_whitespace().collect();
                        if next.starts_with("                ")
                            && next_words.len() >= 2
                            && hex(next_words[0]).is_some()
                            && hex(next_words[1]).is_some()
                        {
                            rest = next_words;
                            lines.next();
                        }
                    }
                    out = match rest.as_slice() {
                        [a, s, more @ ..] if hex(a).is_some() && hex(s).is_some() => {
                            let load = match more {
                                ["load", "address", l, ..] => hex(l),
                                _ => None,
                            };
                            map.sections.push(Section {
                                name,
                                address: hex(a).unwrap_or(0),
                                size: hex(s).unwrap_or(0),
                                load,
                            });
                            Some(map.sections.len() - 1)
                        }
                        _ => None,
                    };
                    continue;
                }
                let Some(o) = out else { continue };
                if indent == 1 {
                    // An input section: ` .text.add 0x… 0x… a.o`, ` *fill*`, ` *(.text*)`.
                    input = None;
                    pending_input = None;
                    let name = words[0];
                    if name.starts_with('*')
                        || matches!(name, "FILL" | "LONG" | "SHORT" | "BYTE" | "QUAD" | "SQUAD" | "ASSERT")
                    {
                        continue;
                    }
                    if words.len() == 1 {
                        pending_input = Some(name);
                        continue;
                    }
                    input = push_input(&mut map, o, &words[1..], line);
                    continue;
                }
                if pending_input.take().is_some()
                    && words.len() >= 3
                    && hex(words[0]).is_some()
                    && hex(words[1]).is_some()
                {
                    input = push_input(&mut map, o, &words, line);
                    continue;
                }
                // `0x… name` (a symbol), `0x… name = expr` or `[!provide] PROVIDE (…)`.
                let Some(address) = hex(words[0]) else { continue };
                if words.len() == 2 && !words[1].starts_with('(') {
                    map.symbols.push(Symbol { name: words[1], address, size: None, input });
                } else if let Some(name) = assignment(line.trim_start()[words[0].len()..].trim()) {
                    map.symbols.push(Symbol { name, address, size: None, input: None });
                }
            }
        }
    }
    gnu_symbol_sizes(&mut map);
    map
}

/// The input section from `0x<address> 0x<size> <object>`.
fn push_input<'a>(map: &mut Map<'a>, out: usize, words: &[&'a str], line: &'a str) -> Option<usize> {
    let (address, size) = (hex(words.first()?)?, hex(words.get(1)?)?);
    // The object is the rest of the line: paths may contain spaces.
    let at = line.find(words[1])? + words[1].len();
    let object = line[at..].trim();
    map.inputs.push(Input { out, object, address, size });
    Some(map.inputs.len() - 1)
}

/// A symbol's size: the distance to the next symbol in its input section, or to the
/// section's end.
fn gnu_symbol_sizes(map: &mut Map) {
    let mut by_input: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, s) in map.symbols.iter().enumerate() {
        if let Some(input) = s.input {
            by_input.entry(input).or_default().push(i);
        }
    }
    for (input, mut syms) in by_input {
        let inp = &map.inputs[input];
        let end = inp.address + inp.size;
        syms.sort_by_key(|&i| map.symbols[i].address);
        for k in 0..syms.len() {
            let a = map.symbols[syms[k]].address;
            let next = syms[k + 1..].iter().map(|&j| map.symbols[j].address).find(|&b| b > a).unwrap_or(end);
            map.symbols[syms[k]].size = Some(next.saturating_sub(a));
        }
    }
}

fn parse_lld(text: &str) -> Map<'_> {
    let mut map = Map { linker: "LLVM lld", ..Map::default() };
    let mut lines = text.lines();
    let Some(header) = lines.find(|l| !l.trim().is_empty()) else { return map };
    let numbers = if header.contains("LMA") { 4 } else { 3 };
    let col = |w: &str| header.find(&format!(" {w} ")).or_else(|| header.find(&format!(" {w}"))).map(|p| p + 1);
    let (Some(in_col), Some(sym_col)) = (col("In"), col("Symbol")) else { return map };
    let mut out = None;
    let mut input = None;
    for line in lines {
        // Skip the number columns, then the indentation tells the row kind.
        let mut rest = line;
        let mut nums = [0u64; 4];
        for n in nums.iter_mut().take(numbers) {
            rest = rest.trim_start();
            let end = rest.find(' ').unwrap_or(rest.len());
            *n = u64::from_str_radix(&rest[..end], 16).unwrap_or(0);
            rest = &rest[end..];
        }
        let item = rest.trim_start();
        if item.is_empty() {
            continue;
        }
        let column = line.len() - item.len();
        let (address, load, size) =
            if numbers == 4 { (nums[0], nums[1], nums[2]) } else { (nums[0], nums[0], nums[1]) };
        let item = item.trim_end();
        if column >= sym_col {
            if let Some(name) = assignment(item) {
                map.symbols.push(Symbol { name, address, size: None, input: None });
            } else {
                map.symbols.push(Symbol { name: item, address, size: Some(size), input });
            }
        } else if column >= in_col {
            input = None;
            let Some(o) = out else { continue };
            if let Some(name) = assignment(item) {
                map.symbols.push(Symbol { name, address, size: None, input: None });
                continue;
            }
            // `file.o:(.text.name)`, `lib.a(member.o):(.text)`, `<internal>:(.bss)`.
            let object = item.rsplit_once(":(").map_or(item, |(o, _)| o);
            map.inputs.push(Input { out: o, object, address, size });
            input = Some(map.inputs.len() - 1);
        } else {
            input = None;
            map.sections.push(Section { name: item, address, size, load: (load != address).then_some(load) });
            out = Some(map.sections.len() - 1);
        }
    }
    map
}

/// Sections without contents in the file: their load address is only nominal.
fn is_nobits(name: &str) -> bool {
    name.starts_with(".bss")
        || name.starts_with(".tbss")
        || name.starts_with(".noinit")
        || name.ends_with(".bss")
        || name == ".stack"
        || name == ".heap"
}

impl Map<'_> {
    fn render(&self) -> String {
        let mut out =
            String::with_capacity(64 * (self.symbols.len() + self.inputs.len() / 4 + self.sections.len()) + 1024);
        line(&mut out, 0, "Linker", self.linker);
        line(&mut out, 0, "Output sections", self.sections.iter().filter(|s| s.size > 0).count());
        line(&mut out, 0, "Symbols", self.symbols.len());
        if self.discarded.0 > 0 {
            line(&mut out, 0, "Discarded", format!("{} input sections, {}", self.discarded.0, bytes(self.discarded.1)));
        }

        if !self.regions.is_empty() {
            let _ = writeln!(out, "\n=== Memory regions ({}) ===", self.regions.len());
            for r in &self.regions {
                let inside = |a: u64| a >= r.origin && a - r.origin < r.length;
                let used: u64 = self
                    .sections
                    .iter()
                    .map(|s| {
                        let run = if inside(s.address) { s.size } else { 0 };
                        let load = match s.load {
                            Some(l) if inside(l) && !inside(s.address) && !is_nobits(s.name) => s.size,
                            _ => 0,
                        };
                        run + load
                    })
                    .sum();
                let _ = writeln!(out, "{}", r.name);
                line(&mut out, 2, "Origin", addr(r.origin));
                line(&mut out, 2, "Length", format!("0x{:x}, {}", r.length, bytes(r.length)));
                let percent = if r.length > 0 { used as f64 * 100.0 / r.length as f64 } else { 0.0 };
                line(&mut out, 2, "Used", format!("{}, {percent:.1} %", bytes(used)));
                if !r.attributes.is_empty() {
                    line(&mut out, 2, "Attributes", r.attributes);
                }
            }
        }

        let mut sections: Vec<(usize, &Section)> =
            self.sections.iter().enumerate().filter(|(_, s)| s.size > 0).collect();
        sections.sort_by_key(|(_, s)| (s.address, s.name));
        let _ = writeln!(out, "\n=== Output sections ({}) ===", sections.len());
        for (_, s) in &sections {
            match s.load {
                Some(l) => {
                    let _ =
                        writeln!(out, "{}: {}, {}, load address {}", s.name, addr(s.address), bytes(s.size), addr(l));
                }
                None => {
                    let _ = writeln!(out, "{}: {}, {}", s.name, addr(s.address), bytes(s.size));
                }
            }
        }

        // Object → output section → bytes. Output sections of the same name (lld
        // and GNU merge differently) are added up.
        let mut by_object: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
        for i in self.inputs.iter().filter(|i| i.size > 0) {
            *by_object.entry(i.object).or_default().entry(self.sections[i.out].name).or_default() += i.size;
        }
        let _ = writeln!(out, "\n=== Size by object ({}) ===", by_object.len());
        for (object, per) in &by_object {
            let total: u64 = per.values().sum();
            let parts: Vec<String> = per.iter().map(|(s, n)| format!("{s} {n}")).collect();
            let _ = writeln!(out, "{object}: {} ({})", bytes(total), parts.join(", "));
        }

        let _ = writeln!(out, "\n=== Size by output section and object ===");
        let mut by_section: BTreeMap<(u64, &str), BTreeMap<&str, u64>> = BTreeMap::new();
        for i in self.inputs.iter().filter(|i| i.size > 0) {
            let s = &self.sections[i.out];
            *by_section.entry((s.address, s.name)).or_default().entry(i.object).or_default() += i.size;
        }
        for ((_, name), objects) in &by_section {
            let total: u64 = objects.values().sum();
            let _ = writeln!(out, "\n--- {name} ({}) ---", bytes(total));
            for (object, n) in objects {
                let _ = writeln!(out, "{object}: {}", bytes(*n));
            }
        }

        let mut symbols: Vec<&Symbol> = self.symbols.iter().collect();
        symbols.sort_by(|a, b| a.name.cmp(b.name).then(a.address.cmp(&b.address)));
        symbols.dedup_by(|a, b| a.name == b.name && a.address == b.address);
        let _ = writeln!(out, "\n=== Symbols ({}) ===", symbols.len());
        for s in symbols.iter().take(MAX_SYMBOLS) {
            let _ = write!(out, "{}: {}", s.name, addr(s.address));
            if let Some(n) = s.size {
                let _ = write!(out, ", {}", bytes(n));
            }
            match s.input.map(|i| &self.inputs[i]) {
                Some(i) => {
                    let _ = writeln!(out, ", {} {}", self.sections[i.out].name, i.object);
                }
                None => out.push('\n'),
            }
        }
        if symbols.len() > MAX_SYMBOLS {
            let _ = writeln!(out, "! {} more symbols are not shown", symbols.len() - MAX_SYMBOLS);
        }
        out
    }
}

fn line(out: &mut String, indent: usize, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:indent$}{:<18} {value}", "", format!("{label}:"));
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/linkmap/", $name))
        };
    }
    const GNU_V1: &str = fixture!("ecu-v1-gnu.map");
    const GNU_V2: &str = fixture!("ecu-v2-gnu.map");
    const LLD_V1: &str = fixture!("ecu-v1-lld.map");
    const LLD_V2: &str = fixture!("ecu-v2-lld.map");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn gnu_map() {
        let m = memory_map(GNU_V1).unwrap();
        for expected in [
            "Linker:            GNU ld",
            "FLASH\n  Origin:            0x08000000\n  Length:            0x40000, 262144 bytes (256 KiB)\n  Used:              ",
            "RAM\n  Origin:            0x20000000\n  Length:            0x10000, 65536 bytes (64 KiB)\n  Used:              16 bytes, 0.0 %",
            "BKPRAM\n  Origin:            0x40024000\n  Length:            0x1000, 4096 bytes (4 KiB)\n  Used:              4 bytes, 0.1 %",
            ".isr_vector: 0x08000000, 32 bytes\n.text: 0x08000020, ",
            ".data: 0x20000000, 4 bytes, load address 0x08",
            "control.o: ",
            "libdrivers.a(can.o): 21 bytes (.bss 4, .text 17)",
            "can_init: 0x08",
            ", 17 bytes, .text libdrivers.a(can.o)",
            "_etext: 0x08",
        ] {
            assert!(m.contains(expected), "{expected}\n---\n{m}");
        }
    }

    #[test]
    fn lld_map() {
        let m = memory_map(LLD_V1).unwrap();
        for expected in [
            "Linker:            LLVM lld",
            ".data: 0x20000000, 4 bytes, load address 0x08",
            "libdrivers.a(can.o): 21 bytes (.bss 4, .text 17)",
            "integrator: 0x20000008, 4 bytes, .bss control.o",
            "\n--- .bss (12 bytes) ---\ncontrol.o: 4 bytes\nlibdrivers.a(can.o): 4 bytes\nmain.o: 4 bytes\n",
        ] {
            assert!(m.contains(expected), "{expected}\n---\n{m}");
        }
    }

    #[test]
    fn a_grown_function_and_a_new_object() {
        for (v1, v2) in [(GNU_V1, GNU_V2), (LLD_V1, LLD_V2)] {
            let (a, b) = (memory_map(v1).unwrap(), memory_map(v2).unwrap());
            let added = lines_only_in(&b, &a);
            assert!(added.iter().any(|l| l.starts_with("diag.o: ")), "{added:?}");
            assert!(added.iter().any(|l| l.starts_with("diag_poll: 0x08")), "{added:?}");
            let grown = lines_only_in(&a, &b);
            assert!(grown.iter().any(|l| l.starts_with("control.o: ")), "{grown:?}");
            // Untouched objects keep their lines.
            assert!(!grown.iter().any(|l| l.starts_with("libdrivers.a(can.o): ")), "{grown:?}");
        }
    }

    #[test]
    fn detection() {
        assert_eq!(detect(GNU_V1.as_bytes()), Some(Flavor::Gnu));
        assert_eq!(detect(LLD_V1.as_bytes()), Some(Flavor::Lld));
        assert_eq!(detect(br#"{"version":3,"sources":["a.ts"],"mappings":"AAAA"}"#), None);
        assert_eq!(detect(b"IAR ELF Linker V9.30\n\n*** MODULE SUMMARY\n"), None);
        assert_eq!(detect(b""), None);
        assert!(memory_map("Memory Configuration\n\ngarbage").is_err());
        for end in (0..GNU_V1.len()).step_by(41) {
            let _ = memory_map(&GNU_V1[..end]);
        }
        for end in (0..LLD_V1.len()).step_by(37) {
            let _ = memory_map(&LLD_V1[..end]);
        }
    }
}
