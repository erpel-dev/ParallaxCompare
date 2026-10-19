//! Excel 97–2003 workbooks (.xls, BIFF8), read with calamine. The text matches
//! what the .xlsx reader shows for the same workbook saved as .xlsx: a header
//! per sheet, a line per row with cells, tab-separated values and `{=…}` formulas.

use std::collections::BTreeMap;
use std::io::Cursor;

use calamine::{Data, Reader, Xls};

use crate::{Error, Result};

pub fn text(data: &[u8]) -> Result<String> {
    // A parser bug on a damaged file must not take the app down.
    std::panic::catch_unwind(|| read(data))
        .map_err(|_| Error::Unsupported("the .xls file could not be parsed".into()))?
}

fn read(data: &[u8]) -> Result<String> {
    let mut book = Xls::new(Cursor::new(data)).map_err(|e| Error::Unsupported(format!(".xls: {e}")))?;
    let mut out = String::new();
    for name in book.sheet_names() {
        out.push_str(&format!("=== Sheet: {name} ===\n"));
        let values = book.worksheet_range(&name).map_err(|e| Error::Unsupported(format!(".xls: {e}")))?;
        let formulas = book.worksheet_formula(&name).unwrap_or_default();
        // (row, column) from the sheet's top left; both ranges start at their first used cell.
        let mut cells: BTreeMap<u32, BTreeMap<u32, (String, String)>> = BTreeMap::new();
        let (r0, c0) = values.start().unwrap_or_default();
        for (r, c, value) in values.cells() {
            let shown = shown(value);
            if !shown.is_empty() {
                cells.entry(r0 + r as u32).or_default().entry(c0 + c as u32).or_default().0 = shown;
            }
        }
        let (r0, c0) = formulas.start().unwrap_or_default();
        for (r, c, formula) in formulas.cells() {
            if !formula.is_empty() {
                cells.entry(r0 + r as u32).or_default().entry(c0 + c as u32).or_default().1 = comparisons(formula);
            }
        }
        // Rows without cells are left out, as an .xlsx has no <row> for them.
        for row in cells.values() {
            let mut line: Vec<String> = Vec::new();
            for (&col, (value, formula)) in row {
                line.resize(col as usize, String::new());
                line.push(if formula.is_empty() { value.clone() } else { format!("{value} {{={formula}}}") });
            }
            out.push_str(&line.join("\t"));
            out.push('\n');
        }
    }
    Ok(out)
}

/// calamine 0.37 swaps PtgGe (0x0C) and PtgGt (0x0D) when it decodes a formula;
/// swap `>` and `>=` back outside string literals and quoted sheet names.
fn comparisons(formula: &str) -> String {
    let mut out = String::with_capacity(formula.len() + 2);
    let mut quote: Option<char> = None;
    let mut chars = formula.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '>') if !out.ends_with('<') => {
                if chars.next_if_eq(&'=').is_some() {
                    out.push('>');
                } else {
                    out.push_str(">=");
                }
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

fn shown(value: &Data) -> String {
    match value {
        Data::Empty => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => number(*f),
        // Dates are serial day numbers, as in the <v> of an .xlsx cell.
        Data::DateTime(d) => number(d.as_f64()),
        Data::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
        Data::Error(e) => e.to_string(),
    }
}

/// A number as spreadsheet applications store it in .xlsx: 15 significant digits,
/// exponent notation for very large and very small values (`1E+020`, `2.5E-005`).
fn number(f: f64) -> String {
    if !f.is_finite() {
        return f.to_string();
    }
    if f == 0.0 {
        return "0".into();
    }
    let sci = format!("{f:.14e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: String| {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
    };
    if (-4..15).contains(&exp) {
        trim(format!("{:.*}", (14 - exp).max(0) as usize, mantissa.parse::<f64>().unwrap_or(0.0) * 10f64.powi(exp)))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}E{sign}{:03}", trim(mantissa.to_string()), exp.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XLS: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/xls-workbook.xls"));
    const XLSX: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/xls-workbook.xlsx"));

    #[test]
    fn numbers() {
        for (f, s) in [
            (12.0, "12"),
            (-7.0, "-7"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.333333333333333"),
            (-7.0 / 3.0, "-2.33333333333333"),
            (1e20, "1E+020"),
            (2.5e-5, "2.5E-005"),
            (123456789.0, "123456789"),
            (0.0001, "0.0001"),
            (46314.0, "46314"),
        ] {
            assert_eq!(number(f), s);
        }
    }

    #[test]
    fn workbook() {
        let v = text(XLS).unwrap();
        for expected in [
            "=== Sheet: Parts ===\nPart\tCount\tPrice\tIn stock\tTotal\n",
            "Bolt M3\t12\t0.1\tTRUE {=TRUE}\t1.2\n",
            "Nut\t-7\t0.333333333333333\t",
            "Sum\t8 {=SUM(B2:B4)}\t\t\t3.36666666666667 {=SUM(E2:E4)}\n",
            "\tBolt M3 x12 {=A2&\" x\"&B2}\t#DIV/0! {=1/0}\tTRUE {=B2>10}\ta>=b {=IF(B3>=3,\"a>=b\",\"\")}\n",
            "Released\t46314\n1E+020\t123456789\t2.5E-005\n",
            "=== Sheet: Notes ü ===\nGrüße aus Köln – €\n\t\tfar away\t",
            "\t42\nline one\nline two\n",
        ] {
            assert!(v.contains(expected), "{expected}\n---\n{v}");
        }
    }

    #[test]
    fn same_as_xlsx() {
        let native = text(XLS).unwrap();
        let ooxml = super::super::office::ooxml_text("xlsx", XLSX).unwrap();
        let (native, ooxml): (Vec<_>, Vec<_>) = (native.lines().collect(), ooxml.lines().collect());
        assert_eq!(native.len(), ooxml.len(), "{native:#?}\n{ooxml:#?}");
        let differ: Vec<_> = native.iter().zip(&ooxml).filter(|(a, b)| a != b).map(|(a, b)| (*a, *b)).collect();
        // LibreOffice writes the constant TRUE as the function TRUE(), stores a filled-down
        // formula once for the range (calamine leaves those out), and turns the boolean
        // result of a comparison into a number.
        assert_eq!(
            differ,
            [
                ("Bolt M3\t12\t0.1\tTRUE {=TRUE}\t1.2", "Bolt M3\t12\t0.1\tTRUE {=TRUE()}\t1.2 {=B2*C2}"),
                ("Washer, heavy\t3\t1.5\tFALSE {=FALSE}\t4.5", "Washer, heavy\t3\t1.5\tFALSE {=FALSE()}\t4.5 {=B3*C3}"),
                (
                    "Nut\t-7\t0.333333333333333\tTRUE {=TRUE}\t-2.33333333333333",
                    "Nut\t-7\t0.333333333333333\tTRUE {=TRUE()}\t-2.33333333333333 {=B4*C4}"
                ),
                (
                    "Label\tBolt M3 x12 {=A2&\" x\"&B2}\t#DIV/0! {=1/0}\tTRUE {=B2>10}\ta>=b {=IF(B3>=3,\"a>=b\",\"\")}",
                    "Label\tBolt M3 x12 {=A2&\" x\"&B2}\t#DIV/0! {=1/0}\t1 {=B2>10}\ta>=b {=IF(B3>=3,\"a>=b\",\"\")}"
                ),
            ]
        );
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(comparisons("A1>=1"), "A1>1");
        assert_eq!(comparisons("A1>1"), "A1>=1");
        assert_eq!(comparisons("A1<>1&\">\"&'a>b'!A1"), "A1<>1&\">\"&'a>b'!A1");
    }

    #[test]
    fn damaged() {
        assert!(text(b"").is_err());
        assert!(text(&XLS[..XLS.len() / 2]).is_err());
        assert!(text(b"\xD0\xCF\x11\xE0 not a compound file").is_err());
        let mut seed = 7u64;
        for _ in 0..300 {
            let mut data = XLS.to_vec();
            for _ in 0..8 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % data.len();
                data[at] = (seed >> 13) as u8;
            }
            let _ = text(&data);
        }
    }
}
