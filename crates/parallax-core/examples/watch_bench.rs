//! Times watching a tree: setup, and what a burst of writes turns into.
//!
//! Usage: watch_bench <folder> <files to write>
use std::sync::Arc;
use std::time::{Duration, Instant};

use parallax_core::source::WatchRoot;
use parallax_core::watch::Watcher;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::PathBuf::from(&args[0]);
    let n: usize = args[1].parse()?;
    let t = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let _w = Watcher::new(
        &[Some(WatchRoot::Tree { dir: dir.clone(), gitignore: true })],
        Arc::new(|_: &str| false),
        move |b| {
            let _ = tx.send((Instant::now(), b));
        },
    )?;
    println!("setup: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);

    let out = dir.join("burst/deep");
    std::fs::create_dir_all(&out)?;
    let t = Instant::now();
    for i in 0..n {
        std::fs::write(out.join(format!("f{i}.o")), b"x")?;
    }
    println!("wrote {n} files in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let mut batches = 0;
    while let Ok((at, b)) = rx.recv_timeout(Duration::from_secs(3)) {
        batches += 1;
        let shown: Vec<_> = b.paths.iter().take(3).collect();
        println!(
            "  batch {batches} after {:.0} ms: full={} {} paths {:?}",
            (at - t).as_secs_f64() * 1e3,
            b.full,
            b.paths.len(),
            shown
        );
    }
    std::fs::remove_dir_all(dir.join("burst"))?;
    Ok(())
}
