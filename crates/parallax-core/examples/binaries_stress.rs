//! Renders every file under the given folders in its format views (wasm, .NET, Android,
//! fonts …) and runs the similar check of each file against itself, with timings:
//! binaries_stress <dir>... [--ext wasm,dll,...]
fn main() -> anyhow::Result<()> {
    use parallax_core::convert::{View, content_equal, render, views_for};
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let exts: Option<Vec<String>> = args.iter().position(|a| a == "--ext").map(|i| {
        let list = args.remove(i + 1);
        args.remove(i);
        list.split(',').map(str::to_string).collect()
    });
    let mut files = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = args.iter().map(Into::into).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(p)? {
                stack.push(e?.path());
            }
        } else if p.is_file() {
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if exts.as_ref().is_none_or(|l| l.contains(&ext)) {
                files.push(p);
            }
        }
    }
    files.sort();
    #[derive(Default)]
    struct Stats {
        files: usize,
        bytes: usize,
        lines: usize,
        failed: usize,
        ms: f64,
        similar_ms: f64,
        slowest: (f64, String, String),
        slowest_similar: (f64, String),
    }
    let mut stats: std::collections::BTreeMap<&'static str, Stats> = Default::default();
    let mut errors: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    for p in &files {
        let data = std::fs::read(p)?;
        let name = p.to_string_lossy().to_string();
        let views = views_for(&name, &data);
        let Some(&first) = views.first() else { continue };
        if matches!(first, View::Hex | View::Text | View::Executable | View::Image | View::Document | View::PrettyXml) {
            continue;
        }
        let s = stats.entry(first.label()).or_default();
        s.files += 1;
        s.bytes += data.len();
        for v in views.iter().copied().filter(|v| !matches!(v, View::Hex | View::Executable | View::Text)) {
            let t = std::time::Instant::now();
            let result = render(v, &name, &data);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            s.ms += ms;
            if ms > s.slowest.0 {
                s.slowest = (ms, v.label().to_string(), name.clone());
            }
            match result {
                Ok(text) => s.lines += text.lines().count(),
                Err(e) => {
                    s.failed += 1;
                    let key: String = format!("{}: {e}", v.label()).chars().take(90).collect();
                    errors.entry(key).or_insert((0, name.clone())).0 += 1;
                }
            }
        }
        // The similar check against a copy: the worst case, as it has to compare everything.
        let copy = data.clone();
        let t = std::time::Instant::now();
        let _ = content_equal(&name, &data, &copy);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        s.similar_ms += ms;
        if ms > s.slowest_similar.0 {
            s.slowest_similar = (ms, name.clone());
        }
    }
    for (kind, s) in &stats {
        println!(
            "{kind}: {} files, {:.1} MB, {} lines, {} failed renders, {:.0} ms in views ({:.2} ms per file), similar check {:.0} ms",
            s.files,
            s.bytes as f64 / 1e6,
            s.lines,
            s.failed,
            s.ms,
            s.ms / s.files as f64,
            s.similar_ms
        );
        println!("  slowest view: {:.1} ms {} {}", s.slowest.0, s.slowest.1, s.slowest.2);
        println!("  slowest similar check: {:.1} ms {}", s.slowest_similar.0, s.slowest_similar.1);
    }
    for (e, (n, example)) in errors {
        println!("{n:>6}  {e}  e.g. {example}");
    }
    Ok(())
}
