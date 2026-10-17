//! Renders every class file under a folder in all class views: class_stress <dir>
fn main() -> anyhow::Result<()> {
    use parallax_core::convert::{View, render};
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(std::env::args().nth(1).expect("a folder"))];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "class") {
                files.push(p);
            }
        }
    }
    let (mut failed, mut bytes, mut lines) = (0, 0, 0);
    let mut errors: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    let start = std::time::Instant::now();
    let mut slowest = (0.0, String::new());
    for p in &files {
        let data = std::fs::read(p)?;
        bytes += data.len();
        let name = p.to_string_lossy();
        let t = std::time::Instant::now();
        for v in [View::ClassOutline, View::Bytecode] {
            match render(v, &name, &data) {
                Ok(text) => lines += text.lines().count(),
                Err(e) => {
                    failed += 1;
                    let key: String = e.to_string().chars().take(70).collect();
                    let entry = errors.entry(key).or_insert((0, name.to_string()));
                    entry.0 += 1;
                }
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if ms > slowest.0 {
            slowest = (ms, name.to_string());
        }
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{} classes, {:.0} MB, {lines} lines, {failed} failed renders, {secs:.2} s ({:.3} ms per class)",
        files.len(),
        bytes as f64 / 1e6,
        secs * 1e3 / files.len() as f64
    );
    println!("slowest: {:.1} ms {}", slowest.0, slowest.1);
    for (e, (n, example)) in errors {
        println!("{n:>6}  {e}  e.g. {example}");
    }
    Ok(())
}
