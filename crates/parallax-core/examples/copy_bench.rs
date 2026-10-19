//! Times the steps behind copying from an archive to a folder, as the app runs them.
//!
//! Usage: copy_bench <archive> <folder with the same tree> <copied dir> <copied file>
//! The folder is changed: `<copied dir>` and `<copied file>` are deleted first.

use std::time::Instant;

use parallax_core::compare::{self, CompareOptions, Control, Filter};
use parallax_core::ops;
use parallax_core::source::{SourceOptions, SourceSpec, open_pair};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [archive, folder, dir, file] = &args[..] else {
        anyhow::bail!("usage: copy_bench <archive> <folder> <copied dir> <copied file>");
    };
    let spec = |p: &str| SourceSpec::Local { path: p.to_string() };
    let opts = CompareOptions::default();
    let time = |label: &str, f: &mut dyn FnMut() -> anyhow::Result<String>| -> anyhow::Result<()> {
        let t = Instant::now();
        let note = f()?;
        println!("{:>9.1} ms  {label} {note}", t.elapsed().as_secs_f64() * 1000.0);
        Ok(())
    };
    let _ = std::fs::remove_dir_all(std::path::Path::new(folder).join(dir));
    let _ = std::fs::remove_file(std::path::Path::new(folder).join(file));

    // Viewing one file without a compare first, e.g. a file pair opened from the CLI.
    let (l, _) = open_pair(&spec(archive), &spec(folder), SourceOptions::default(), false)?;
    time("view one file, cold", &mut || Ok(format!("({} bytes)", l.vfs.read(&l.path(file))?.len())))?;
    time("view one file, warm", &mut || Ok(format!("({} bytes)", l.vfs.read(&l.path(file))?.len())))?;
    drop(l);

    let (l, r) = open_pair(&spec(archive), &spec(folder), SourceOptions::default(), false)?;
    time("full compare", &mut || {
        let s = compare::compare(&l, &r, &opts, &Control::default())?.summary;
        Ok(format!("({} files, {} left only)", s.files, s.left_only))
    })?;
    time("copy file", &mut || {
        Ok(format!("({} copied)", ops::copy(&l, &r, std::slice::from_ref(file), &Filter::none(), &Default::default())?))
    })?;
    time("  refresh file", &mut || {
        Ok(format!("({:?})", compare::compare_path(&l, &r, file, &opts, &Control::default())?.root.status))
    })?;
    time("copy folder", &mut || {
        Ok(format!("({} copied)", ops::copy(&l, &r, std::slice::from_ref(dir), &Filter::none(), &Default::default())?))
    })?;
    time("  refresh folder", &mut || {
        Ok(format!("({:?})", compare::compare_path(&l, &r, dir, &opts, &Control::default())?.root.status))
    })?;

    let all = tempfile::tempdir()?;
    let (l, r) = open_pair(&spec(archive), &spec(&all.path().display().to_string()), SourceOptions::default(), false)?;
    time("copy everything, fresh session", &mut || {
        let all: Vec<String> = l.vfs.list(&l.path(""))?.into_iter().map(|e| e.name).collect();
        Ok(format!("({} copied)", ops::copy(&l, &r, &all, &Filter::none(), &Default::default())?))
    })?;
    time("  refresh everything", &mut || {
        let s = compare::compare(&l, &r, &opts, &Control::default())?.summary;
        Ok(format!("({} same)", s.same))
    })?;
    Ok(())
}
