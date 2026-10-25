use std::fs;
use std::process::Command;

fn parallax(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_parallax")).args(args).arg("--color=never").output().unwrap();
    (out.status.code().unwrap(), String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn folders_and_files() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("x.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(b.path().join("x.txt"), "one\n2\nthree\n").unwrap();
    fs::write(b.path().join("new.txt"), "n").unwrap();
    let (code, out) = parallax(&[a.path().to_str().unwrap(), b.path().to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.contains("≠ x.txt"), "{out}");
    assert!(out.contains("→ new.txt"), "{out}");

    let (code, out) = parallax(&[a.path().join("x.txt").to_str().unwrap(), b.path().join("x.txt").to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.contains("@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"), "{out}");

    let (code, _) = parallax(&[a.path().to_str().unwrap(), a.path().to_str().unwrap()]);
    assert_eq!(code, 0);

    let (code, _) = parallax(&["/definitely/missing", a.path().to_str().unwrap()]);
    assert_eq!(code, 2);
}

#[test]
fn hunk_headers_follow_unified_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
    fs::write(&a, "1\n2\n3\n").unwrap();
    fs::write(&b, "1\n2\n3\n4\n").unwrap();
    let (_, out) = parallax(&[a.to_str().unwrap(), b.to_str().unwrap(), "-U", "0"]);
    assert!(out.contains("@@ -3,0 +4,1 @@\n+4\n"), "{out}");
    let (_, out) = parallax(&[b.to_str().unwrap(), a.to_str().unwrap(), "-U", "0"]);
    assert!(out.contains("@@ -4,1 +3,0 @@\n-4\n"), "{out}");
}

#[test]
fn dcm_odx_and_pdx_content_comparisons() {
    let fixture = |name: &str| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../parallax-core/tests/fixtures/automotive").join(name)
    };
    for (ext, formatted_suffix, expected) in [
        ("dcm", "reformatted", "Value: 850"),
        ("odx-d", "reformatted", "BIT-LENGTH"),
        ("pdx", "repacked", "diagnostics/engine.odx-d"),
    ] {
        let (left, same, changed) = (
            fixture(&format!("engine-v1.{ext}")),
            fixture(&format!("engine-v1-{formatted_suffix}.{ext}")),
            fixture(&format!("engine-v2.{ext}")),
        );
        let (code, out) = parallax(&[left.to_str().unwrap(), same.to_str().unwrap()]);
        assert_eq!(code, 0, "{ext}: {out}");
        let (code, out) = parallax(&[left.to_str().unwrap(), changed.to_str().unwrap()]);
        assert_eq!(code, 1, "{ext}: {out}");
        assert!(out.contains(expected), "{ext}: {out}");
        if ext != "pdx" {
            assert_eq!(parallax(&[left.to_str().unwrap(), same.to_str().unwrap(), "--raw"]).0, 1);
        }
    }
}

#[test]
fn score_configurations_definitions_and_flatbuffers() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../parallax-core/tests/fixtures/score");
    for prefix in ["communication", "launch", "logging", "parameters"] {
        let a = dir.join(format!("{prefix}-v1.json"));
        let b = dir.join(format!("{prefix}-reordered.json"));
        let c = dir.join(format!("{prefix}-v2.json"));
        assert_eq!(parallax(&[a.to_str().unwrap(), b.to_str().unwrap()]).0, 0, "{prefix}");
        assert_eq!(parallax(&[a.to_str().unwrap(), c.to_str().unwrap()]).0, 1, "{prefix}");
        assert_eq!(parallax(&[a.to_str().unwrap(), b.to_str().unwrap(), "--raw"]).0, 1, "{prefix}");
    }
    for ext in ["proto", "fidl", "fdepl", "rsl"] {
        let a = dir.join(format!("definitions-v1.{ext}"));
        let b = dir.join(format!("definitions-formatted.{ext}"));
        let c = dir.join(format!("definitions-v2.{ext}"));
        assert_eq!(parallax(&[a.to_str().unwrap(), b.to_str().unwrap()]).0, 0, "{ext}");
        assert_eq!(parallax(&[a.to_str().unwrap(), c.to_str().unwrap()]).0, 1, "{ext}");
    }
    let schema = dir.join("config.bfbs");
    let a = dir.join("data-v1.bin");
    for (name, expected) in
        [("data-repacked.bin", 0), ("data-size-prefixed.bin", 0), ("data-v2.bin", 1), ("data-unknown.bin", 2)]
    {
        let b = dir.join(name);
        let (code, text) = parallax(&[a.to_str().unwrap(), b.to_str().unwrap(), "--schema", schema.to_str().unwrap()]);
        assert_eq!(code, expected, "{name}: {text}");
    }
    assert_eq!(parallax(&[a.to_str().unwrap(), dir.join("data-repacked.bin").to_str().unwrap(), "--raw"]).0, 1);
}

/// Entries that could not be read make the exit status 2, also with --json, after
/// the result is printed.
#[test]
fn unreadable_entries_exit_with_2() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("broken.zip"), "not a zip").unwrap();
    fs::write(b.path().join("broken.zip"), "not a zip either").unwrap();
    let (code, out) = parallax(&[a.path().to_str().unwrap(), b.path().to_str().unwrap()]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("1 errors"), "{out}");
    let (code, out) = parallax(&[a.path().to_str().unwrap(), b.path().to_str().unwrap(), "--json"]);
    assert_eq!(code, 2);
    assert!(out.contains("\"errors\":1"), "{out}");
}

#[test]
fn include_patterns() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for (dir, text) in [(&a, "1"), (&b, "2")] {
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/Main.java"), "same").unwrap();
        fs::write(dir.path().join("src/notes.txt"), text).unwrap();
    }
    let (left, right) = (a.path().to_str().unwrap(), b.path().to_str().unwrap());
    assert_eq!(parallax(&[left, right]).0, 1);
    let (code, out) = parallax(&[left, right, "--include", "*.java"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("1 files compared"), "{out}");
}

fn merge(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_parallax")).arg("merge").args(args).output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code().unwrap(), text(&out.stdout), text(&out.stderr))
}

#[test]
fn three_way_merge() {
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
    fs::write(p("base"), "a\nb\nc\nd\ne\n").unwrap();
    fs::write(p("local"), "a\nB\nc\nd\ne\n").unwrap();
    fs::write(p("remote"), "a\nb\nc\nd\nE\n").unwrap();
    let (code, out, _) = merge(&[&p("base"), &p("local"), &p("remote")]);
    assert_eq!((code, out.as_str()), (0, "a\nB\nc\nd\nE\n"));

    fs::write(p("remote"), "a\nX\nc\nd\ne\n").unwrap();
    let (code, _, err) = merge(&[
        &p("base"),
        &p("local"),
        &p("remote"),
        "-o",
        &p("out"),
        "-L",
        "mine",
        "-L",
        "old",
        "-L",
        "theirs",
        "--style",
        "diff3",
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("1 conflict left"), "{err}");
    assert_eq!(
        fs::read_to_string(p("out")).unwrap(),
        "a\n<<<<<<< mine\nB\n||||||| old\nb\n=======\nX\n>>>>>>> theirs\nc\nd\ne\n"
    );

    fs::write(p("bin"), b"\0\x01").unwrap();
    assert_eq!(merge(&[&p("base"), &p("bin"), &p("remote")]).0, 2);
}

/// `--strategy mergiraf` takes mergiraf's result; a stand-in mergiraf here writes
/// the remote side with a marker line, so its output is recognisable.
#[cfg(unix)]
#[test]
fn syntax_strategy_uses_mergiraf() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
    fs::write(
        p("mergiraf"),
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'mergiraf 9.9.9'; exit 0; }\n\
         remote=$4; shift 4\nwhile [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=$2; shift; done\n\
         { echo solved; cat \"$remote\"; } > \"$out\"\n",
    )
    .unwrap();
    fs::set_permissions(p("mergiraf"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(p("base.rs"), "a\n").unwrap();
    fs::write(p("local.rs"), "b\n").unwrap();
    fs::write(p("remote.rs"), "c\n").unwrap();
    let files = [p("base.rs"), p("local.rs"), p("remote.rs")];
    let files: Vec<&str> = files.iter().map(String::as_str).collect();
    assert_eq!(merge(&files).0, 1, "a conflict for the line merge");
    let (code, out, _) = merge(&[&files[..], &["--strategy", "mergiraf", "--mergiraf", &p("mergiraf")]].concat());
    assert_eq!((code, out.as_str()), (0, "solved\nc\n"));
    let (code, _, err) = merge(&[&files[..], &["--strategy", "mergiraf", "--mergiraf", &p("missing")]].concat());
    assert_eq!(code, 2);
    assert!(err.contains("cargo install mergiraf"), "{err}");
}

/// `--strategy syntax` merges struct fields both sides added with the built-in engine.
#[test]
fn syntax_strategy_is_built_in() {
    let dir = tempfile::tempdir().unwrap();
    let p = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
    fs::write(p("base.rs"), "struct C {\n    a: u8,\n}\n").unwrap();
    fs::write(p("local.rs"), "struct C {\n    a: u8,\n    b: u8,\n}\n").unwrap();
    fs::write(p("remote.rs"), "struct C {\n    a: u8,\n    c: u8,\n}\n").unwrap();
    let files = [p("base.rs"), p("local.rs"), p("remote.rs")];
    let files: Vec<&str> = files.iter().map(String::as_str).collect();
    assert_eq!(merge(&files).0, 1);
    let (code, out, _) = merge(&[&files[..], &["--strategy", "syntax"]].concat());
    assert_eq!((code, out.as_str()), (0, "struct C {\n    a: u8,\n    b: u8,\n    c: u8,\n}\n"));
}
