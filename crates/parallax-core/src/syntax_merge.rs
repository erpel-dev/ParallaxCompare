//! Syntax-aware merging through mergiraf (<https://mergiraf.org>), when it is
//! installed. mergiraf is a separate program (GPL-3.0); it is run, never linked.
//!
//! mergiraf parses the three versions with tree-sitter and merges their syntax
//! trees where Git's line merge leaves conflicts: both sides adding struct
//! fields, imports or JSON keys at the same place, or changing different
//! arguments on the same line. Everything else it merges as Git does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::{Error, Result};

/// Environment variable naming the mergiraf program, ahead of `PATH`.
pub const ENV: &str = "PARALLAX_MERGIRAF";

/// An installed mergiraf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mergiraf {
    pub path: PathBuf,
    /// As `mergiraf --version` prints it: "0.20.0".
    pub version: String,
}

/// mergiraf's result for one file.
#[derive(Debug, Clone)]
pub struct SyntaxMerge {
    pub data: Vec<u8>,
    /// Conflicts left in `data`, with Git's markers.
    pub conflicts: usize,
}

const NOT_FOUND: &str =
    "mergiraf was not found. Install it (cargo install mergiraf, or your system's package) or give its path.";

impl Mergiraf {
    /// The mergiraf at `configured`, else the one named by `PARALLAX_MERGIRAF`,
    /// else the first on `PATH`.
    pub fn find(configured: Option<&Path>) -> Result<Mergiraf> {
        let path = match configured.filter(|p| !p.as_os_str().is_empty()) {
            Some(p) => p.to_path_buf(),
            None => match std::env::var_os(ENV).filter(|v| !v.is_empty()) {
                Some(p) => PathBuf::from(p),
                None => on_path().ok_or_else(|| Error::Unsupported(NOT_FOUND.into()))?,
            },
        };
        if !path.is_file() {
            return Err(Error::Unsupported(format!("{NOT_FOUND} ({} is not a file)", path.display())));
        }
        let out = crate::process::run(
            Command::new(&path).arg("--version"),
            "mergiraf",
            "mergiraf --version",
            Duration::from_secs(10),
        )?;
        let version = parse_version(&out).ok_or_else(|| {
            Error::Unsupported(format!("{} does not look like mergiraf: {}", path.display(), out.trim()))
        })?;
        Ok(Mergiraf { path, version })
    }

    /// Merges three versions of a file. `name` is the file's name or path, from
    /// which mergiraf picks the language; `labels` (local, base, remote) go after
    /// the conflict markers. mergiraf falls back to a line merge itself on parse
    /// errors, at its time limit and for text that is not UTF-8.
    pub fn merge(
        &self,
        base: &[u8],
        local: &[u8],
        remote: &[u8],
        name: &str,
        labels: [&str; 3],
        timeout: Duration,
    ) -> Result<SyntaxMerge> {
        let dir = tempfile::tempdir().map_err(|e| Error::io("temporary folder", e))?;
        // The inputs keep the file's extension, so the language is found from either.
        let file_name = Path::new(name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = Path::new(&file_name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let paths = ["base", "local", "remote", "merged"].map(|n| dir.path().join(format!("{n}{ext}")));
        for (path, data) in paths.iter().zip([base, local, remote]) {
            std::fs::write(path, data).map_err(|e| Error::io(path.display().to_string(), e))?;
        }
        let mut cmd = Command::new(&self.path);
        cmd.arg("merge")
            .args([&paths[0], &paths[1], &paths[2]])
            .arg("-o")
            .arg(&paths[3])
            .args(["-x", labels[0], "-s", labels[1], "-y", labels[2]])
            .arg("-t")
            .arg(timeout.as_millis().to_string())
            .current_dir(dir.path())
            // "mergiraf=0" turns it into git merge-file; Git's variables would point
            // it at the user's repository.
            .env_remove("mergiraf");
        if !file_name.is_empty() {
            cmd.arg("-p").arg(&file_name);
        }
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(key);
            }
        }
        // Its own time limit covers the merge; this one only a program that hangs.
        let limit = timeout + Duration::from_secs(10);
        let (status, _, err) = crate::process::run_status(&mut cmd, "mergiraf", "mergiraf merge", limit)?;
        // 0: merged, 1: conflicts left. Anything else is a failure.
        if !matches!(status.code(), Some(0 | 1)) {
            let detail = String::from_utf8_lossy(&err);
            return Err(Error::Unsupported(format!("mergiraf merge failed ({status}): {}", detail.trim())));
        }
        let data = std::fs::read(&paths[3]).map_err(|e| Error::io("mergiraf's result", e))?;
        let conflicts = count_conflicts(&data);
        Ok(SyntaxMerge { data, conflicts })
    }
}

fn on_path() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["mergiraf.exe", "mergiraf"] } else { &["mergiraf"] };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

/// "mergiraf 0.20.0" → "0.20.0".
fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().next()?.trim();
    let (name, version) = line.rsplit_once(' ')?;
    (name.eq_ignore_ascii_case("mergiraf") && version.starts_with(|c: char| c.is_ascii_digit()))
        .then(|| version.to_string())
}

/// Conflicts in a merged text: lines starting with Git's opening marker.
pub fn count_conflicts(data: &[u8]) -> usize {
    data.split(|&b| b == b'\n').filter(|l| l.starts_with(b"<<<<<<<")).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_conflicts() {
        assert_eq!(parse_version("mergiraf 0.20.0\n").as_deref(), Some("0.20.0"));
        assert_eq!(parse_version("git version 2.45"), None);
        assert_eq!(count_conflicts(b"a\n<<<<<<< l\nx\n=======\ny\n>>>>>>> r\n<<<<<<<\n"), 2);
    }

    #[test]
    fn missing_program_explains_how_to_install() {
        let e = Mergiraf::find(Some(Path::new("/nonexistent/mergiraf"))).unwrap_err().to_string();
        assert!(e.contains("cargo install mergiraf"), "{e}");
    }

    /// A stand-in for mergiraf: answers `--version` and "merges" by taking the
    /// local side, exiting 1 when the remote side holds the word CONFLICT.
    #[cfg(unix)]
    pub(crate) fn fake_mergiraf(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("mergiraf");
        let script = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "mergiraf 9.9.9"; exit 0; fi
[ "$1" = "merge" ] || exit 2
base=$2; local=$3; remote=$4; shift 4
while [ $# -gt 0 ]; do [ "$1" = "-o" ] && out=$2; shift; done
if grep -q CONFLICT "$remote"; then printf '<<<<<<< l\nx\n=======\ny\n>>>>>>> r\n' > "$out"; exit 1; fi
cp "$local" "$out"
"#;
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn runs_the_program_and_reads_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let m = Mergiraf::find(Some(&fake_mergiraf(dir.path()))).unwrap();
        assert_eq!(m.version, "9.9.9");
        let labels = ["l", "b", "r"];
        let clean = m.merge(b"a\n", b"local\n", b"remote\n", "x.rs", labels, Duration::from_secs(5)).unwrap();
        assert_eq!((clean.data.as_slice(), clean.conflicts), (&b"local\n"[..], 0));
        let conflicted = m.merge(b"a\n", b"l\n", b"CONFLICT\n", "x.rs", labels, Duration::from_secs(5)).unwrap();
        assert_eq!(conflicted.conflicts, 1);
    }

    /// With a real mergiraf installed (on PATH or in PARALLAX_MERGIRAF): both
    /// sides' struct fields are offered as the solution of that conflict, and
    /// the conflict in the function stays.
    #[test]
    fn real_mergiraf_solutions_map_onto_conflicts() {
        let Ok(m) = Mergiraf::find(None) else {
            eprintln!("mergiraf not installed: skipped");
            return;
        };
        let base = "struct Config {\n    name: String,\n    port: u16,\n}\n\nfn untouched() -> u32 {\n    1\n}\n\nfn speed() -> u32 {\n    compute(4)\n}\n";
        let local = base
            .replace("    port: u16,\n", "    port: u16,\n    timeout_ms: u64,\n")
            .replace("compute(4)", "compute_slow(4)");
        let remote = base
            .replace("    port: u16,\n", "    port: u16,\n    verbose: bool,\n")
            .replace("compute(4)", "compute_fast(4)");
        let labels = ["local", "base", "remote"];
        let out = m
            .merge(base.as_bytes(), local.as_bytes(), remote.as_bytes(), "config.rs", labels, Duration::from_secs(10))
            .unwrap();
        assert_eq!(out.conflicts, 1);
        let (b, l, r) = crate::merge_session::decode3(base.as_bytes(), local.as_bytes(), remote.as_bytes());
        let mut s = crate::merge_session::MergeSession::new(b, l, r, Default::default());
        let outcome = s.set_syntax_result(&String::from_utf8(out.data).unwrap());
        assert_eq!((outcome.solved, outcome.conflicts, outcome.mapped), (1, 1, true));
        let first = s.conflicts()[0].block as usize;
        assert_eq!(s.syntax_suggestion(first), Some("    timeout_ms: u64,\n    verbose: bool,\n"));
    }
}
