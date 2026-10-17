//! Prints the content views of a file, with timings: views <file> [part of a view label]
fn main() -> anyhow::Result<()> {
    use parallax_core::convert::{View, render, views_for};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0])?;
    for v in views_for(&args[0], &data) {
        if v == View::Hex || args.get(1).is_some_and(|w| !v.label().to_lowercase().contains(&w.to_lowercase())) {
            continue;
        }
        let t = std::time::Instant::now();
        let text = render(v, &args[0], &data)?;
        eprintln!("--- {} ({} lines, {:.2} ms)", v.label(), text.lines().count(), t.elapsed().as_secs_f64() * 1e3);
        println!("{text}");
    }
    Ok(())
}
