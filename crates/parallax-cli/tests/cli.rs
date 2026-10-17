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
