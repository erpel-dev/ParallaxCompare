//! Runs the image metadata and media summary views over files or folders, with
//! timings: `media_stress <path>…`. With `--pair a b` it times the ≈ similar check.
use std::time::Instant;

use parallax_core::convert::{View, content_equal, render, views_for};

fn map(path: &std::path::Path) -> Option<memmap2::Mmap> {
    let f = std::fs::File::open(path).ok()?;
    // SAFETY: read-only mapping of a file nobody writes during the run.
    unsafe { memmap2::Mmap::map(&f) }.ok()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--pair") {
        let (a, b) = (map(args[1].as_ref()).unwrap(), map(args[2].as_ref()).unwrap());
        let t = Instant::now();
        let r = content_equal(&args[1], &a, &b);
        println!("{:?} in {:.2} ms", r, t.elapsed().as_secs_f64() * 1e3);
        return;
    }
    let (mut files, mut failed, mut total, mut slowest) = (0usize, 0usize, 0f64, (0f64, String::new()));
    for root in &args {
        for entry in ignore::WalkBuilder::new(root).standard_filters(false).follow_links(true).build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let Some(data) = map(path) else { continue };
            let name = path.to_string_lossy();
            for view in views_for(&name, &data) {
                if !matches!(view, View::ImageMetadata | View::MediaSummary) {
                    continue;
                }
                files += 1;
                let t = Instant::now();
                let r = render(view, &name, &data);
                let ms = t.elapsed().as_secs_f64() * 1e3;
                total += ms;
                if ms > slowest.0 {
                    slowest = (ms, name.to_string());
                }
                match r {
                    Ok(text) if args.len() == 1 && files == 1 && entry.depth() == 0 => println!("{text}"),
                    Ok(_) => {}
                    Err(e) => {
                        failed += 1;
                        eprintln!("{name}: {e}");
                    }
                }
                if args.len() == 1 && entry.depth() == 0 {
                    eprintln!("{} {name}: {ms:.2} ms", view.label());
                }
            }
        }
    }
    println!("{files} renders, {failed} failed, {total:.1} ms total, slowest {:.2} ms ({})", slowest.0, slowest.1);
}
