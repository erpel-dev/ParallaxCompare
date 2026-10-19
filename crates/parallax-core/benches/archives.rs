//! Comparing archives: opening large ones, and entries that differ.
//!
//! `cargo bench -p parallax-core --bench archives` builds the archives in a
//! temporary folder and prints, per case, the median time of a few folder
//! compares and the peak memory of the process running them (Linux); each case
//! runs in a process of its own.
//! `PARALLAX_BENCH_MB` sets the size of the content per archive (default 200).

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use parallax_core::compare::{self, CompareOptions, Control, Status};
use parallax_core::source::{SourceOptions, SourceSpec, open_pair};

const RUNS: usize = 5;

/// A small linear congruential generator: the same data on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }

    /// Text like source code: words and line breaks.
    fn text(&mut self, n: usize) -> String {
        let words = ["let", "value", "match", "self", "return", "data", "=", "+", "(", ")", "{", "}", "if", "else"];
        let mut out = String::with_capacity(n + 16);
        while out.len() < n {
            out.push_str(words[self.next() as usize % words.len()]);
            out.push(if self.next().is_multiple_of(9) { '\n' } else { ' ' });
        }
        out
    }
}

fn write_zip(path: &Path, method: zip::CompressionMethod, files: &[(String, Vec<u8>)]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    for (name, data) in files {
        zip.start_file(name.as_str(), options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn peak_mb() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    Some(line.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024)
}

fn run(name: &str, left: &Path, right: &Path, expect: Status) {
    let spec = |p: &Path| SourceSpec::parse(&p.display().to_string());
    let opts = CompareOptions::default();
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            let (l, r) = open_pair(&spec(left), &spec(right), SourceOptions::default(), false).unwrap();
            let result = compare::compare(&l, &r, &opts, &Control::default()).unwrap();
            assert_eq!(result.root.status, expect, "{name}");
            start.elapsed()
        })
        .collect();
    times.sort();
    let peak = peak_mb().map(|mb| format!("{mb:>6} MB peak")).unwrap_or_default();
    println!("{name:<52} {:>8.1} ms  {peak}", times[RUNS / 2].as_secs_f64() * 1000.0);
}

/// Writes the archives of all cases into `dir`.
fn build(dir: &Path, mb: usize) {
    let path = |name: &str| dir.join(name);
    let mut random = Random(1);

    // 100 files of incompressible data, stored: opening them is most of the work.
    let entry = mb * 1024 * 1024 / 100;
    let binary: Vec<(String, Vec<u8>)> = (0..100).map(|i| (format!("bin/f{i}.bin"), random.bytes(entry))).collect();
    write_zip(&path("binary.zip"), zip::CompressionMethod::Stored, &binary);
    fs::copy(path("binary.zip"), path("binary-copy.zip")).unwrap();

    // 200 text files, deflated, and variants: changed near the start, near the end,
    // and with CRLF line endings.
    let entry = mb * 1024 * 1024 / 200;
    let text: Vec<(String, Vec<u8>)> =
        (0..200).map(|i| (format!("src/f{i}.txt"), random.text(entry).into_bytes())).collect();
    let deflate = zip::CompressionMethod::Deflated;
    write_zip(&path("text.zip"), deflate, &text);
    let changed = |at: fn(usize) -> usize| -> Vec<(String, Vec<u8>)> {
        text.iter()
            .map(|(n, d)| {
                let mut d = d.clone();
                d.insert(at(d.len()), b'#');
                (n.clone(), d)
            })
            .collect()
    };
    write_zip(&path("text-start.zip"), deflate, &changed(|_| 100));
    write_zip(&path("text-end.zip"), deflate, &changed(|len| len - 100));
    let crlf: Vec<(String, Vec<u8>)> =
        text.iter().map(|(n, d)| (n.clone(), String::from_utf8_lossy(d).replace('\n', "\r\n").into_bytes())).collect();
    write_zip(&path("text-crlf.zip"), deflate, &crlf);
}

const CASES: [(&str, &str, &str, Status); 4] = [
    ("identical zips, 100 stored binary files", "binary.zip", "binary-copy.zip", Status::Same),
    ("text zips, every file changed near the start", "text.zip", "text-start.zip", Status::Different),
    ("text zips, every file changed near the end", "text.zip", "text-end.zip", Status::Different),
    ("text zips, LF vs CRLF (≈ similar)", "text.zip", "text-crlf.zip", Status::Similar),
];

fn main() {
    // A case run in a process of its own, so its peak memory is its own.
    if let Ok(dir) = std::env::var("PARALLAX_BENCH_DIR") {
        let name = std::env::var("PARALLAX_BENCH_CASE").unwrap();
        let (name, left, right, expect) = CASES.into_iter().find(|c| c.0 == name).unwrap();
        run(name, &Path::new(&dir).join(left), &Path::new(&dir).join(right), expect);
        return;
    }
    // `cargo bench` passes `--bench`; a filter argument selects cases by name.
    let filter = std::env::args().skip(1).find(|a| !a.starts_with("--"));
    let mb: usize = std::env::var("PARALLAX_BENCH_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
    let dir = tempfile::tempdir().unwrap();
    build(dir.path(), mb);
    println!("{mb} MB of content per archive, median of {RUNS} compares\n");
    for (name, ..) in CASES {
        if filter.as_deref().is_none_or(|f| name.contains(f)) {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .env("PARALLAX_BENCH_DIR", dir.path())
                .env("PARALLAX_BENCH_CASE", name)
                .status()
                .unwrap();
            assert!(status.success(), "{name}");
        }
    }
}
