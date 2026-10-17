//! End-to-end tests: real folders, archives and Git repositories compared through the public API.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use parallax_core::compare::{self, CompareMode, CompareOptions, Control, Status};
use parallax_core::source::{SourceOptions, SourceSpec, open_pair};
use parallax_core::vfs::EntryKind;

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
}

fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    for (path, content) in files {
        zip.start_file(*path, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    buf.into_inner()
}

fn statuses(left: &str, right: &str, opts: &CompareOptions) -> Vec<(String, Status)> {
    let (l, r) = open_pair(&SourceSpec::parse(left), &SourceSpec::parse(right), SourceOptions::default(), false)
        .expect("sources open");
    let result = compare::compare(&l, &r, opts, &Control::default()).expect("compare runs");
    compare::flatten(&result.root)
}

fn status_of(list: &[(String, Status)], path: &str) -> Status {
    list.iter().find(|(p, _)| p == path).unwrap_or_else(|| panic!("{path} missing in {list:?}")).1
}

#[test]
fn local_folders() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), &[("same.txt", "x"), ("diff.txt", "one"), ("crlf.txt", "a\r\nb\r\n"), ("only/left.txt", "l")]);
    write(b.path(), &[("same.txt", "x"), ("diff.txt", "two"), ("crlf.txt", "a\nb\n"), ("right.txt", "r")]);
    let list = statuses(&a.path().display().to_string(), &b.path().display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "same.txt"), Status::Same);
    assert_eq!(status_of(&list, "diff.txt"), Status::Different);
    assert_eq!(status_of(&list, "crlf.txt"), Status::Same);
    assert_eq!(status_of(&list, "only"), Status::LeftOnly);
    assert_eq!(status_of(&list, "only/left.txt"), Status::LeftOnly);
    assert_eq!(status_of(&list, "right.txt"), Status::RightOnly);
    // Folders sort before files.
    assert_eq!(list[0].0, "only");

    let strict = CompareOptions { ignore_line_endings: false, ..Default::default() };
    let list = statuses(&a.path().display().to_string(), &b.path().display().to_string(), &strict);
    assert_eq!(status_of(&list, "crlf.txt"), Status::Different);

    let quick = CompareOptions { mode: CompareMode::Quick, ..Default::default() };
    let list = statuses(&a.path().display().to_string(), &b.path().display().to_string(), &quick);
    assert_eq!(status_of(&list, "right.txt"), Status::RightOnly);
}

#[test]
fn zip_against_folder_and_nested_archives() {
    let dir = tempfile::tempdir().unwrap();
    let files = [("bin/app.txt", "v2"), ("readme.md", "hello")];
    fs::write(dir.path().join("release.zip"), zip_bytes(&files)).unwrap();
    let folder = dir.path().join("dist");
    write(&folder, &[("bin/app.txt", "v1"), ("readme.md", "hello")]);

    let zip = dir.path().join("release.zip").display().to_string();
    let list = statuses(&zip, &folder.display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "readme.md"), Status::Same);
    assert_eq!(status_of(&list, "bin/app.txt"), Status::Different);

    // A path inside the archive.
    let list = statuses(&format!("{zip}!/bin"), &folder.join("bin").display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "app.txt"), Status::Different);

    // An archive nested in a folder expands like a folder on both sides.
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    fs::write(left.join("fonts.zip"), zip_bytes(&[("a.ttf", "A"), ("b.ttf", "B")])).unwrap();
    fs::write(right.join("fonts.zip"), zip_bytes(&[("a.ttf", "A"), ("b.ttf", "B2"), ("c.ttf", "C")])).unwrap();
    let list = statuses(&left.display().to_string(), &right.display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "fonts.zip"), Status::Different);
    assert_eq!(status_of(&list, "fonts.zip/a.ttf"), Status::Same);
    assert_eq!(status_of(&list, "fonts.zip/b.ttf"), Status::Different);
    assert_eq!(status_of(&list, "fonts.zip/c.ttf"), Status::RightOnly);
}

#[test]
fn seven_zip_and_tar() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    write(&src, &[("a/one.txt", "1"), ("two.txt", "2")]);
    sevenz_rust2::compress_to_path(&src, dir.path().join("pack.7z")).unwrap();

    let tar_gz = dir.path().join("pack.tar.gz");
    let enc = flate2::write::GzEncoder::new(fs::File::create(&tar_gz).unwrap(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all(".", &src).unwrap();
    tar.into_inner().unwrap().finish().unwrap();

    let other = dir.path().join("other");
    write(&other, &[("a/one.txt", "1"), ("two.txt", "changed")]);
    for archive in ["pack.7z", "pack.tar.gz"] {
        let path = dir.path().join(archive).display().to_string();
        let list = statuses(&path, &other.display().to_string(), &CompareOptions::default());
        assert_eq!(status_of(&list, "a/one.txt"), Status::Same, "{archive}");
        assert_eq!(status_of(&list, "two.txt"), Status::Different, "{archive}");
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git is installed");
    assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
}

fn make_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    write(
        repo,
        &[
            ("src/cart.ts", "export const MAX = 50;\n"),
            ("src/catalog/list.ts", "list\n"),
            ("vendor/big/lib.js", "lib\n"),
            (".gitignore", "build/\n"),
        ],
    );
    fs::write(repo.join("assets.zip"), zip_bytes(&[("logo.svg", "<svg/>")])).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "initial"]);
    git(repo, &["checkout", "-q", "-b", "feature"]);
    write(repo, &[("src/cart.ts", "export const MAX = 100;\n"), ("src/discount.ts", "d\n")]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "feature work"]);
    git(repo, &["checkout", "-q", "main"]);
    write(repo, &[("README.md", "main moved on\n")]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "main work"]);
    dir
}

#[test]
fn git_branches() {
    let dir = make_repo();
    let repo = dir.path().display().to_string();
    let left = SourceSpec::parse(&format!("{repo}@main"));
    let right = SourceSpec::parse(&format!("{repo}@feature"));
    let (l, r) = open_pair(&left, &right, SourceOptions::default(), false).unwrap();
    assert!(!l.info.writable);
    let result = compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap();
    let list = compare::flatten(&result.root);
    assert_eq!(status_of(&list, "src/cart.ts"), Status::Different);
    assert_eq!(status_of(&list, "src/discount.ts"), Status::RightOnly);
    assert_eq!(status_of(&list, "README.md"), Status::LeftOnly);
    // Unchanged folders are recognised by tree id and not walked.
    assert_eq!(status_of(&list, "vendor"), Status::Same);
    assert!(!list.iter().any(|(p, _)| p == "vendor/big"));
    assert_eq!(status_of(&list, "src/catalog"), Status::Same);
    assert!(result.summary.identical_subtrees >= 2);

    // ...and can be expanded on demand.
    let sub = compare::compare_path(&l, &r, "vendor", &CompareOptions::default(), &Control::default()).unwrap();
    assert_eq!(sub.root.children[0].name, "big");

    // Compared against the merge-base, main's own README change disappears.
    let (l, r) = open_pair(&left, &right, SourceOptions::default(), true).unwrap();
    assert_eq!(l.info.rev.as_deref(), Some("main (merge-base)"));
    let result = compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap();
    let list = compare::flatten(&result.root);
    assert!(!list.iter().any(|(p, _)| p == "README.md"));
    assert_eq!(status_of(&list, "src/cart.ts"), Status::Different);

    // Sub-folder syntax.
    let sub = SourceSpec::parse(&format!("{repo}@feature:src"));
    let (l, r) =
        open_pair(&SourceSpec::parse(&format!("{repo}@main:src")), &sub, SourceOptions::default(), false).unwrap();
    let list =
        compare::flatten(&compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap().root);
    assert_eq!(status_of(&list, "cart.ts"), Status::Different);

    // An archive stored in Git expands too.
    let entries = l.vfs.list("assets.zip").unwrap();
    assert_eq!(entries[0].name, "logo.svg");
    assert_eq!(l.vfs.stat("assets.zip").unwrap().kind, EntryKind::Archive);
}

#[test]
fn git_worktree_against_revision() {
    let dir = make_repo();
    let repo = dir.path().display().to_string();
    write(dir.path(), &[("build/out.bin", "ignored"), ("src/catalog/list.ts", "edited\n")]);
    let (l, r) = open_pair(
        &SourceSpec::parse(&format!("{repo}@HEAD")),
        &SourceSpec::GitWorktree { repo: repo.clone() },
        SourceOptions::default(),
        false,
    )
    .unwrap();
    assert!(r.info.writable);
    let result = compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap();
    let list = compare::flatten(&result.root);
    assert_eq!(status_of(&list, "src/catalog/list.ts"), Status::Different);
    assert_eq!(status_of(&list, "src/cart.ts"), Status::Same);
    assert!(!list.iter().any(|(p, _)| p.starts_with("build") || p.starts_with(".git/")));
}

#[test]
fn copy_between_sides() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), &[("dir/x.txt", "x"), ("y.txt", "y")]);
    let (l, r) = open_pair(
        &SourceSpec::parse(&a.path().display().to_string()),
        &SourceSpec::parse(&b.path().display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    assert_eq!(parallax_core::ops::copy(&l, &r, "dir").unwrap(), 1);
    parallax_core::ops::copy(&l, &r, "y.txt").unwrap();
    let list =
        compare::flatten(&compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap().root);
    assert!(list.iter().all(|(_, s)| *s == Status::Same), "{list:?}");
    parallax_core::ops::delete(&r, "dir").unwrap();
    assert!(!b.path().join("dir").exists());
}
