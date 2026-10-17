//! Prints the GGUF views of a file: gguf_views <file> [view]
fn main() -> anyhow::Result<()> {
    use parallax_core::convert::{View, render};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&args[0])?;
    let views = [View::Model, View::ChatTemplate, View::ChatPreview, View::Metadata, View::Tokenizer, View::Tensors];
    for v in views {
        if args.get(1).is_some_and(|w| !v.label().to_lowercase().contains(&w.to_lowercase())) {
            continue;
        }
        let t = std::time::Instant::now();
        let text = render(v, &args[0], &data)?;
        eprintln!("--- {} ({} lines, {:.1} ms)", v.label(), text.lines().count(), t.elapsed().as_secs_f64() * 1e3);
        println!("{text}");
    }
    Ok(())
}
