//! Renders every SQLite, Parquet/Arrow, safetensors and ONNX file under the given
//! files or folders in their views, memory-mapped as the app reads them, and reports
//! counts, failures and timings: data_stress <path>...
//! `LIST=1` prints each file found. With `--similar <left> <right>` it times the ≈ similar check of two files instead.
fn main() -> anyhow::Result<()> {
    use parallax_core::convert::{View, content_equal, render, views_for};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let map = |p: &std::path::Path| -> anyhow::Result<memmap2::Mmap> {
        let f = std::fs::File::open(p)?;
        Ok(unsafe { memmap2::Mmap::map(&f)? })
    };
    if args.first().is_some_and(|a| a == "--similar") {
        let (l, r) = (map(args[1].as_ref())?, map(args[2].as_ref())?);
        let t = std::time::Instant::now();
        let reason = content_equal(&args[1], &l, &r);
        println!("{:.1} ms  {reason:?}", t.elapsed().as_secs_f64() * 1e3);
        return Ok(());
    }
    let ours =
        [View::DatabaseSchema, View::DatabaseRows, View::DataSummary, View::Model, View::Tensors, View::ModelGraph];
    let mut stack: Vec<std::path::PathBuf> = args.iter().map(Into::into).collect();
    let (mut files, mut failed, mut lines) = (0, 0, 0);
    let mut slowest = (0.0, String::new());
    let mut errors: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    let start = std::time::Instant::now();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
            continue;
        }
        if !p.is_file() || p.metadata().map_or(0, |m| m.len()) == 0 {
            continue;
        }
        let Ok(data) = map(&p) else { continue };
        let name = p.to_string_lossy().to_string();
        let views: Vec<View> = views_for(&name, &data).into_iter().filter(|v| ours.contains(v)).collect();
        if views.is_empty() || (views[0] == View::Model && name.ends_with(".gguf")) {
            continue;
        }
        files += 1;
        if std::env::var("LIST").is_ok() {
            println!("{}  {name}", views[0].label());
        }
        for v in views {
            let t = std::time::Instant::now();
            match render(v, &name, &data) {
                Ok(text) => lines += text.lines().count(),
                Err(e) => {
                    failed += 1;
                    let key: String = e.to_string().chars().take(70).collect();
                    errors.entry(key).or_insert((0, name.clone())).0 += 1;
                }
            }
            let ms = t.elapsed().as_secs_f64() * 1e3;
            if ms > slowest.0 {
                slowest = (ms, format!("{} of {name}", v.label()));
            }
            if args.len() == 1 && files == 1 && std::env::var("SHOW").is_ok() {
                println!("{}", render(v, &name, &data).unwrap_or_default());
            }
        }
    }
    println!("{files} files, {lines} lines, {failed} failed renders, {:.2} s", start.elapsed().as_secs_f64());
    println!("slowest: {:.1} ms {}", slowest.0, slowest.1);
    for (e, (n, example)) in errors {
        println!("{n:>6}  {e}  e.g. {example}");
    }
    Ok(())
}
