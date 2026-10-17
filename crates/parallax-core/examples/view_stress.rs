//! Renders every file under the given folders in all its content views except Text and
//! Hex, and runs the ≈ similar check of each file against itself; prints counts,
//! failures and the slowest files: view_stress <dir or file>…
fn main() -> anyhow::Result<()> {
    use parallax_core::convert::{View, content_equal, has_content_rule, render, views_for};
    let mut files = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = std::env::args().skip(1).map(Into::into).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(p)? {
                stack.push(e?.path());
            }
        } else if p.is_file() {
            files.push(p);
        }
    }
    files.sort();
    let mut per_view: std::collections::BTreeMap<&str, (usize, usize, f64, usize)> = Default::default();
    let mut errors: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    let mut slowest: Vec<(f64, String)> = Vec::new();
    let mut similar = (0usize, 0.0f64);
    for p in &files {
        let Ok(data) = std::fs::read(p) else { continue };
        let name = p.to_string_lossy().to_string();
        for v in views_for(&name, &data) {
            if matches!(v, View::Text | View::Hex | View::PrettyXml | View::PrettyJson) {
                continue;
            }
            let t = std::time::Instant::now();
            let result = render(v, &name, &data);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let e = per_view.entry(v.label()).or_default();
            e.0 += 1;
            e.2 += ms;
            match result {
                Ok(text) => e.3 += text.lines().count(),
                Err(err) => {
                    e.1 += 1;
                    let key: String = err.to_string().chars().take(80).collect();
                    errors.entry(key).or_insert((0, name.clone())).0 += 1;
                }
            }
            slowest.push((ms, format!("{} {} ({} KB)", v.label(), name, data.len() / 1024)));
        }
        if has_content_rule(&name) {
            let t = std::time::Instant::now();
            let _ = content_equal(&name, &data, &data);
            similar.0 += 1;
            similar.1 += t.elapsed().as_secs_f64() * 1e3;
        }
    }
    println!("{} files", files.len());
    for (view, (n, failed, ms, lines)) in &per_view {
        println!("{view}: {n} rendered, {failed} failed, {lines} lines, {ms:.1} ms total");
    }
    println!("similar check: {} files, {:.1} ms total", similar.0, similar.1);
    for (e, (n, example)) in &errors {
        println!("  {n}× {e}  (e.g. {example})");
    }
    slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (ms, what) in slowest.iter().take(5) {
        println!("  {ms:.1} ms  {what}");
    }
    Ok(())
}
