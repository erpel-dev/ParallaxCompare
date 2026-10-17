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
    assert_eq!(status_of(&list, "crlf.txt"), Status::Similar);
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
    // A tracked file inside an ignored folder stays visible, like in `git status`.
    write(dir.path(), &[("build/keep.txt", "tracked\n")]);
    git(dir.path(), &["add", "-f", "build/keep.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "force-add"]);
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
    // With core.autocrlf (the Windows default) checked-out files may have CRLF.
    assert!(matches!(status_of(&list, "src/cart.ts"), Status::Same | Status::Similar));
    assert!(matches!(status_of(&list, "build/keep.txt"), Status::Same | Status::Similar));
    assert!(!list.iter().any(|(p, _)| p == "build/out.bin" || p.starts_with(".git/")));

    // A folder opened directly is shown completely, even when it is ignored itself.
    let (l, r) = open_pair(
        &SourceSpec::parse(&format!("{repo}@HEAD:build")),
        &SourceSpec::parse(&dir.path().join("build").display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    let list =
        compare::flatten(&compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap().root);
    assert_eq!(status_of(&list, "keep.txt"), Status::Same);
    assert_eq!(status_of(&list, "out.bin"), Status::RightOnly);
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

#[cfg(unix)]
#[test]
fn symlinks_compare_by_target() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    write(repo, &[("real/a.txt", "a"), ("other.txt", "o")]);
    symlink("real", repo.join("dir-link")).unwrap();
    symlink("other.txt", repo.join("file-link")).unwrap();
    // A link to a parent folder must not be walked (it would never end).
    symlink("..", repo.join("real/up")).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "links"]);

    let spec = repo.display().to_string();
    let (l, r) = open_pair(
        &SourceSpec::parse(&format!("{spec}@HEAD")),
        &SourceSpec::GitWorktree { repo: spec.clone() },
        SourceOptions::default(),
        false,
    )
    .unwrap();
    let result = compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap();
    assert_eq!(result.root.status, Status::Same, "{:?}", compare::flatten(&result.root));
    assert_eq!(r.vfs.stat("dir-link").unwrap().kind, EntryKind::Symlink);

    // Plain folders (no .gitignore snapshot) treat links the same way.
    let copy = tempfile::tempdir().unwrap();
    write(copy.path(), &[("real/a.txt", "a"), ("other.txt", "o")]);
    symlink("real", copy.path().join("dir-link")).unwrap();
    symlink("elsewhere.txt", copy.path().join("file-link")).unwrap();
    let list = statuses(&spec, &copy.path().display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "dir-link"), Status::Same);
    assert_eq!(status_of(&list, "file-link"), Status::Different);
    assert_eq!(status_of(&list, "real/up"), Status::LeftOnly);

    // Copying recreates the link instead of writing its target as a file.
    let (l, r) = open_pair(
        &SourceSpec::parse(&spec),
        &SourceSpec::parse(&copy.path().display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    parallax_core::ops::copy(&l, &r, "file-link").unwrap();
    assert_eq!(fs::read_link(copy.path().join("file-link")).unwrap(), Path::new("other.txt"));
}

fn docx(text: &str, minute: u8) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let time = zip::DateTime::from_date_and_time(2026, 1, 1, 12, minute, 0).unwrap();
    let opts = zip::write::SimpleFileOptions::default().last_modified_time(time);
    zip.start_file("word/document.xml", opts).unwrap();
    let xml = format!(
        r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">A &amp; B</w:t></w:r></w:p></w:body></w:document>"#
    );
    zip.write_all(xml.as_bytes()).unwrap();
    zip.finish().unwrap();
    buf.into_inner()
}

fn png(compression: image::codecs::png::CompressionType, red: u8) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([red, 10, 10, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    let encoder =
        image::codecs::png::PngEncoder::new_with_quality(&mut out, compression, image::codecs::png::FilterType::Sub);
    img.write_with_encoder(encoder).unwrap();
    out.into_inner()
}

#[test]
fn documents_and_images_compare_by_content() {
    use image::codecs::png::CompressionType::{Best, Fast};
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("same.docx"), docx("Hello", 1)).unwrap();
    fs::write(b.path().join("same.docx"), docx("Hello", 2)).unwrap();
    fs::write(a.path().join("changed.docx"), docx("Hello", 1)).unwrap();
    fs::write(b.path().join("changed.docx"), docx("Hallo", 1)).unwrap();
    fs::write(a.path().join("same.png"), png(Fast, 200)).unwrap();
    fs::write(b.path().join("same.png"), png(Best, 200)).unwrap();
    fs::write(a.path().join("changed.png"), png(Fast, 200)).unwrap();
    fs::write(b.path().join("changed.png"), png(Fast, 201)).unwrap();
    assert_ne!(fs::read(a.path().join("same.png")).unwrap(), fs::read(b.path().join("same.png")).unwrap());

    let (pa, pb) = (a.path().display().to_string(), b.path().display().to_string());
    let list = statuses(&pa, &pb, &CompareOptions::default());
    assert_eq!(status_of(&list, "same.docx"), Status::Similar);
    assert_eq!(status_of(&list, "changed.docx"), Status::Different);
    assert_eq!(status_of(&list, "same.png"), Status::Similar);
    assert_eq!(status_of(&list, "changed.png"), Status::Different);

    let bytes_only = CompareOptions { compare_contents: false, ..Default::default() };
    let list = statuses(&pa, &pb, &bytes_only);
    assert_eq!(status_of(&list, "same.docx"), Status::Different);

    // The document view shows the text, entities decoded.
    let text = parallax_core::convert::render(
        parallax_core::convert::View::Document,
        "a.docx",
        &fs::read(a.path().join("same.docx")).unwrap(),
    )
    .unwrap();
    assert_eq!(text, "Hello\nA & B\n");
}
