//! Renders every file under the given paths in its content views (Text and Hex
//! aside) and runs the ≈ similar check of each file against itself, with timings:
//! devtext_stress <file or folder>...
use std::time::Instant;

use parallax_core::convert::{View, content_equal, has_content_rule, render, views_for};

fn main() -> anyhow::Result<()> {
    let mut files = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = std::env::args().skip(1).map(Into::into).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p)? {
                stack.push(e?.path());
            }
        } else if p.is_file() {
            files.push(p);
        }
    }
    files.sort();
    let (mut rendered, mut failed, mut similar_checks) = (0, 0, 0);
    let mut slowest: Vec<(f64, String)> = Vec::new();
    let start = Instant::now();
    for p in &files {
        let data = std::fs::read(p)?;
        let name = p.to_string_lossy().to_string();
        for v in views_for(&name, &data) {
            if matches!(v, View::Text | View::Hex | View::Image | View::Document | View::Executable) {
                continue;
            }
            let t = Instant::now();
            match render(v, &name, &data) {
                Ok(text) => {
                    rendered += 1;
                    let ms = t.elapsed().as_secs_f64() * 1e3;
                    slowest.push((
                        ms,
                        format!(
                            "{} {name} ({} MB in, {} lines out)",
                            v.label(),
                            data.len() / 1_000_000,
                            text.lines().count()
                        ),
                    ));
                }
                Err(e) => {
                    failed += 1;
                    if failed <= 20 {
                        println!("! {} {name}: {e}", v.label());
                    }
                }
            }
        }
        if has_content_rule(&name) {
            let t = Instant::now();
            let r = content_equal(&name, &data, &data);
            similar_checks += 1;
            slowest.push((t.elapsed().as_secs_f64() * 1e3, format!("similar check {name} -> {r:?}")));
        }
    }
    slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!(
        "{} files, {rendered} views rendered, {failed} failed, {similar_checks} similar checks, {:.2} s",
        files.len(),
        start.elapsed().as_secs_f64()
    );
    for (ms, what) in slowest.iter().take(12) {
        println!("{ms:>9.2} ms  {what}");
    }
    Ok(())
}
