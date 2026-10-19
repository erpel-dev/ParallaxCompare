//! End-to-end tests: real folders, archives and Git repositories compared through the public API.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use parallax_core::compare::{self, CompareMode, CompareOptions, Control, Status};
use parallax_core::ops::{self, Removal};
use parallax_core::progress::Progress;
use parallax_core::source::{Source, SourceOptions, SourceSpec, open_pair};
use parallax_core::vfs::EntryKind;

fn copy(from: &Source, to: &Source, path: &str) -> parallax_core::Result<u64> {
    ops::copy(from, to, &[path.to_string()], &Progress::default())
}

fn delete(source: &Source, path: &str) -> parallax_core::Result<()> {
    ops::delete(source, &[path.to_string()], Removal::Permanent, &Progress::default()).map(|_| ())
}

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

fn zip_raw(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    for (path, content) in files {
        zip.start_file(*path, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(content).unwrap();
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
    assert_eq!(copy(&l, &r, "dir").unwrap(), 1);
    copy(&l, &r, "y.txt").unwrap();
    let list =
        compare::flatten(&compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap().root);
    assert!(list.iter().all(|(_, s)| *s == Status::Same), "{list:?}");
    delete(&r, "dir").unwrap();
    assert!(!b.path().join("dir").exists());
}

/// Files that moved to another folder: compared and copied across paths, also into a zip.
#[test]
fn compare_and_copy_across_folders() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), &[("cfg/config.json", "{\"a\": 1}"), ("src/util/s.rs", "x\r\n")]);
    write(b.path(), &[("lib/config.json", "{\"a\": 2}"), ("src/common/s.rs", "x\n")]);
    fs::write(b.path().join("pack.zip"), zip_bytes(&[("in/config.json", "old")])).unwrap();
    let spec = |p: &Path| SourceSpec::parse(&p.display().to_string());
    let (l, r) = open_pair(&spec(a.path()), &spec(b.path()), SourceOptions::default(), false).unwrap();
    let opts = CompareOptions { ignore_line_endings: false, compare_contents: false, ..CompareOptions::default() };
    let pair = |x: &str, y: &str, o: &CompareOptions| compare::compare_pair(&l, &r, x, y, o).unwrap().0;
    assert_eq!(pair("cfg/config.json", "lib/config.json", &opts), Status::Different);
    assert_eq!(pair("src/util/s.rs", "src/common/s.rs", &opts), Status::Different);
    let eol = CompareOptions { ignore_line_endings: true, ..opts.clone() };
    assert_eq!(pair("src/util/s.rs", "src/common/s.rs", &eol), Status::Similar);
    assert!(compare::compare_pair(&l, &r, "cfg", "lib", &opts).is_err(), "folders are not paired");

    ops::copy_file_as(&l, &r, "cfg/config.json", "lib/config.json", &Progress::default()).unwrap();
    assert_eq!(fs::read_to_string(b.path().join("lib/config.json")).unwrap(), "{\"a\": 1}");
    assert!(!b.path().join("cfg").exists(), "nothing is created at the source's path");
    assert_eq!(pair("cfg/config.json", "lib/config.json", &opts), Status::Same);
    assert!(ops::copy_file_as(&l, &r, "cfg", "lib", &Progress::default()).is_err());

    // Into an archive on the other side.
    let (l, z) =
        open_pair(&spec(a.path()), &spec(&b.path().join("pack.zip")), SourceOptions::default(), false).unwrap();
    ops::copy_file_as(&l, &z, "cfg/config.json", "in/config.json", &Progress::default()).unwrap();
    assert_eq!(compare::compare_pair(&l, &z, "cfg/config.json", "in/config.json", &opts).unwrap().0, Status::Same);
}

#[test]
fn copy_out_of_solid_and_non_solid_7z() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    write(&src, &[("a/one.txt", "1"), ("a/two.txt", "22"), ("b/three.txt", "333"), ("empty.txt", "")]);
    for solid in [true, false] {
        let archive = dir.path().join(format!("pack-{solid}.7z"));
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
        if solid {
            writer.push_source_path(&src, |_| true).unwrap();
        } else {
            writer.push_source_path_non_solid(&src, |_| true).unwrap();
        }
        writer.finish().unwrap();

        let target = tempfile::tempdir().unwrap();
        let (l, r) = open_pair(
            &SourceSpec::parse(&archive.display().to_string()),
            &SourceSpec::parse(&target.path().display().to_string()),
            SourceOptions::default(),
            false,
        )
        .unwrap();
        for child in l.vfs.list(&l.path("")).unwrap() {
            copy(&l, &r, &child.name).unwrap();
        }
        assert_eq!(fs::read_to_string(target.path().join("b/three.txt")).unwrap(), "333");
        assert_eq!(fs::read(target.path().join("empty.txt")).unwrap(), b"");
        let list =
            compare::flatten(&compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap().root);
        assert_eq!(list.len(), 6, "{list:?}");
        assert!(list.iter().all(|(_, s)| *s == Status::Same), "solid: {solid}, {list:?}");
    }
}

#[test]
fn copy_into_folder_with_gitignore_updates_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    write(&repo, &[(".gitignore", "*.log\n"), ("keep.txt", "old")]);
    let zip = dir.path().join("release.zip");
    fs::write(
        &zip,
        zip_bytes(&[("new/deep/a.txt", "a"), ("keep.txt", "new"), ("x.log", "log"), (".gitignore", "*.log\n")]),
    )
    .unwrap();
    let (l, r) = open_pair(
        &SourceSpec::parse(&zip.display().to_string()),
        &SourceSpec::parse(&repo.display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    let refresh = |path: &str| {
        compare::compare_path(&l, &r, path, &CompareOptions::default(), &Control::default()).unwrap().root.status
    };
    assert_eq!(refresh("new"), Status::LeftOnly);
    // A file in a folder that does not exist yet on the right.
    copy(&l, &r, "new/deep/a.txt").unwrap();
    assert_eq!(refresh("new"), Status::Same);
    copy(&l, &r, "keep.txt").unwrap();
    assert_eq!(refresh("keep.txt"), Status::Same);
    // Ignore rules still hide what they match.
    copy(&l, &r, "x.log").unwrap();
    assert!(repo.join("x.log").exists());
    assert_eq!(refresh("x.log"), Status::LeftOnly);
    delete(&r, "new").unwrap();
    assert_eq!(refresh("new"), Status::LeftOnly);
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
    self::copy(&l, &r, "file-link").unwrap();
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

/// An archive of the given type holding `files`.
fn archive_bytes(kind: &str, files: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write as _;
    if kind == "zip" {
        return zip_bytes(files);
    }
    if kind.starts_with("7z") {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), files);
        let mut writer = sevenz_rust2::ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
        if kind == "7z" {
            writer.push_source_path(dir.path(), |_| true).unwrap();
        } else {
            writer.push_source_path_non_solid(dir.path(), |_| true).unwrap();
        }
        return writer.finish().unwrap().into_inner();
    }
    let mut tar = tar::Builder::new(Vec::new());
    for (path, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, path, content.as_bytes()).unwrap();
    }
    let raw = tar.into_inner().unwrap();
    match kind {
        "tar" => raw,
        "tar.gz" => {
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(&raw).unwrap();
            enc.finish().unwrap()
        }
        "tar.bz2" => {
            let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            enc.write_all(&raw).unwrap();
            enc.finish().unwrap()
        }
        "tar.xz" => {
            let mut enc = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
            enc.write_all(&raw).unwrap();
            enc.finish().unwrap()
        }
        "tar.zst" => ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest),
        _ => unreachable!(),
    }
}

#[test]
fn copy_into_and_delete_from_archives() {
    for kind in ["zip", "7z", "7z-non-solid", "tar", "tar.gz", "tar.bz2", "tar.xz", "tar.zst"] {
        let dir = tempfile::tempdir().unwrap();
        let ext = kind.trim_end_matches("-non-solid");
        let archive = dir.path().join(format!("pack.{ext}"));
        let original =
            [("a/one.txt", "1"), ("a/two.txt", "22"), ("keep.txt", "k"), ("gone/x.txt", "x"), ("empty.txt", "")];
        fs::write(&archive, archive_bytes(kind, &original)).unwrap();
        let folder = dir.path().join("folder");
        write(
            &folder,
            &[
                ("a/one.txt", "1"),
                ("a/two.txt", "changed"),
                ("new/deep/n.txt", "n"),
                ("other.txt", "o"),
                ("empty.txt", ""),
            ],
        );

        let spec = |p: &Path| SourceSpec::parse(&p.display().to_string());
        let (l, r) = open_pair(&spec(&archive), &spec(&folder), SourceOptions::default(), false).unwrap();
        let opts = CompareOptions::default();
        let refresh = |path: &str| compare::compare_path(&l, &r, path, &opts, &Control::default()).unwrap().root.status;
        assert!(l.info.writable, "{kind}");
        assert_eq!(copy(&r, &l, "a/two.txt").unwrap(), 1, "{kind}");
        assert_eq!(refresh("a/two.txt"), Status::Same, "{kind}");
        // An empty file first, then folders: see the 7z header check below.
        copy(&r, &l, "empty.txt").unwrap();
        assert_eq!(copy(&r, &l, "new").unwrap(), 1, "{kind}");
        assert_eq!(refresh("new"), Status::Same, "{kind}");
        delete(&l, "gone").unwrap();
        delete(&l, "keep.txt").unwrap();

        // A fresh look at the file on disk.
        let list = statuses(&archive.display().to_string(), &folder.display().to_string(), &opts);
        let expect = [
            ("a/one.txt", Status::Same),
            ("a/two.txt", Status::Same),
            ("new/deep/n.txt", Status::Same),
            ("other.txt", Status::RightOnly),
        ];
        for (path, status) in expect {
            assert_eq!(status_of(&list, path), status, "{kind}: {path}");
        }
        assert!(!list.iter().any(|(p, _)| p.starts_with("gone") || p == "keep.txt"), "{kind}: {list:?}");
        assert_eq!(status_of(&list, "empty.txt"), Status::Same, "{kind}");
        if ext == "7z" {
            // 7-Zip rejects the header when a folder follows the last empty file, since
            // sevenz-rust2 then writes the empty-file vector too short.
            let data = fs::read(&archive).unwrap();
            let parsed =
                sevenz_rust2::Archive::read(&mut std::io::Cursor::new(&data[..]), &sevenz_rust2::Password::empty())
                    .unwrap();
            let without_data: Vec<_> = parsed.files.iter().filter(|f| !f.has_stream).collect();
            assert!(without_data.iter().any(|f| !f.is_directory), "{kind}: the empty file has no data");
            assert!(!without_data.last().unwrap().is_directory, "{kind}: {}", without_data.last().unwrap().name);
        }
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 2, "{kind}: temporary files left: {leftovers:?}");
    }
}

#[test]
fn write_into_nested_archives() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left");
    write(&left, &[("lib/x.txt", "x")]);
    let inner = archive_bytes("tar.gz", &[("x.txt", "old")]);
    let outer = {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        zip.start_file("readme.md", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"hi").unwrap();
        zip.start_file("lib.tar.gz", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(&inner).unwrap();
        zip.finish().unwrap();
        buf.into_inner()
    };
    let right = dir.path().join("right");
    fs::create_dir_all(&right).unwrap();
    fs::write(right.join("bundle.zip"), outer).unwrap();
    // Left: a folder named like the right side's nested archive path.
    fs::create_dir_all(left.join("bundle.zip")).unwrap();
    fs::rename(left.join("lib"), left.join("bundle.zip/lib.tar.gz")).unwrap();

    let spec = |p: &Path| SourceSpec::parse(&p.display().to_string());
    let (l, r) = open_pair(&spec(&left), &spec(&right), SourceOptions::default(), false).unwrap();
    copy(&l, &r, "bundle.zip/lib.tar.gz/x.txt").unwrap();
    let list = statuses(&left.display().to_string(), &right.display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "bundle.zip/lib.tar.gz/x.txt"), Status::Same);
    assert_eq!(status_of(&list, "bundle.zip/readme.md"), Status::RightOnly);
}

#[test]
fn git_revisions_stay_read_only() {
    let repo = make_repo();
    let other = tempfile::tempdir().unwrap();
    write(other.path(), &[("new.txt", "n")]);
    let (l, r) = open_pair(
        &SourceSpec::parse(&format!("{}@main", repo.path().display())),
        &SourceSpec::parse(&other.path().display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    assert!(!l.info.writable);
    assert!(matches!(copy(&r, &l, "new.txt"), Err(parallax_core::Error::Unsupported(_))));
}

#[test]
fn zip_rewrite_keeps_entries_as_stored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pack.zip");
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    // Info-ZIP's Unix UID/GID field, which the `zip` tool writes for every entry.
    let mut options = zip::write::FullFileOptions::default();
    options.add_extra_field(0x7875, [1, 4, 0, 0, 0, 0, 4, 0, 0, 0, 0], false).unwrap();
    zip.start_file("old.txt", options).unwrap();
    zip.write_all(b"old").unwrap();
    zip.set_comment("release 1").unwrap();
    zip.finish().unwrap();
    fs::write(&path, buf.into_inner()).unwrap();
    let folder = dir.path().join("folder");
    write(&folder, &[("new.txt", "new")]);

    let (l, r) = open_pair(
        &SourceSpec::parse(&path.display().to_string()),
        &SourceSpec::parse(&folder.display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    copy(&r, &l, "new.txt").unwrap();

    let mut zip = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(zip.comment(), b"release 1");
    let old = zip.by_name("old.txt").unwrap();
    let extra = old.extra_data().unwrap_or_default();
    assert!(extra.windows(2).any(|w| w == [0x75, 0x78]), "{extra:?}");
    drop(old);
    assert_eq!(std::io::read_to_string(zip.by_name("new.txt").unwrap()).unwrap(), "new");
}

#[test]
fn cancelled_copy_into_an_archive_leaves_it_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("pack.zip");
    fs::write(&zip, zip_bytes(&[("a.txt", "a")])).unwrap();
    let before = fs::read(&zip).unwrap();
    let folder = dir.path().join("folder");
    write(&folder, &[("b.txt", "b"), ("c/d.txt", "d")]);
    let (l, r) = open_pair(
        &SourceSpec::parse(&zip.display().to_string()),
        &SourceSpec::parse(&folder.display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    let progress = Progress::default();
    progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let paths = ["b.txt".to_string(), "c".to_string()];
    assert!(matches!(ops::copy(&r, &l, &paths, &progress), Err(parallax_core::Error::Cancelled)));
    assert_eq!(fs::read(&zip).unwrap(), before);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2, "no temporary file left");

    // Not cancelled: both paths go in, with one rewrite counted by the progress.
    let progress = Progress::default();
    assert_eq!(ops::copy(&r, &l, &paths, &progress).unwrap(), 2);
    let list = statuses(&zip.display().to_string(), &folder.display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "c/d.txt"), Status::Same);
    let done = progress.done.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(done, progress.total.load(std::sync::atomic::Ordering::Relaxed));
}

#[cfg(target_os = "linux")]
#[test]
fn delete_moves_to_the_trash() {
    let dir = tempfile::tempdir().unwrap();
    // A trash of its own, on the same file system as the files.
    // SAFETY: no other test reads XDG_DATA_HOME.
    unsafe { std::env::set_var("XDG_DATA_HOME", dir.path().join("data")) };
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    write(&a, &[("x.txt", "x")]);
    write(&b, &[("x.txt", "x"), ("folder/y.txt", "y")]);
    let (_, r) = open_pair(
        &SourceSpec::parse(&a.display().to_string()),
        &SourceSpec::parse(&b.display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    let paths = ["x.txt".to_string(), "folder".to_string()];
    let not_trashed = ops::delete(&r, &paths, Removal::Trash, &Progress::default()).unwrap();
    assert!(not_trashed.is_empty());
    assert!(!b.join("x.txt").exists() && !b.join("folder").exists());
    let trashed: Vec<_> =
        fs::read_dir(dir.path().join("data/Trash/files")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(trashed.len(), 2, "{trashed:?}");
}

#[test]
fn refresh_sees_changes_made_outside() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("pack.zip");
    fs::write(&zip, zip_bytes(&[("a.txt", "a")])).unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    write(&repo, &[("a.txt", "a")]);
    let (l, r) = open_pair(
        &SourceSpec::parse(&zip.display().to_string()),
        &SourceSpec::parse(&repo.display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap();
    let compare_all =
        || compare::flatten(&compare::compare(&l, &r, &CompareOptions::default(), &Control::default()).unwrap().root);
    assert_eq!(status_of(&compare_all(), "a.txt"), Status::Same);

    // Another program replaces the archive and adds a file to the folder.
    fs::write(&zip, zip_bytes(&[("a.txt", "changed")])).unwrap();
    write(&repo, &[("new.txt", "n")]);
    l.vfs.changed(&l.path(""));
    r.vfs.changed(&r.path(""));
    let list = compare_all();
    assert_eq!(status_of(&list, "a.txt"), Status::Different);
    assert_eq!(status_of(&list, "new.txt"), Status::RightOnly);
}

/// Java classes that differ only in debug info are "similar", also inside jars.
#[test]
fn java_classes_and_jars() {
    let fixture =
        |name: &str| fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/java").join(name)).unwrap();
    let (v1, shifted, v2, item) = (
        fixture("OrderService-v1.class"),
        fixture("OrderService-v1-shifted.class"),
        fixture("OrderService-v2.class"),
        fixture("OrderService-Item.class"),
    );
    let dir = tempfile::tempdir().unwrap();
    let class = "com/example/shop/OrderService.class";
    let item_class = "com/example/shop/OrderService$Item.class";
    fs::write(dir.path().join("app-1.0.jar"), zip_raw(&[(class, &v1), (item_class, &item)])).unwrap();
    fs::write(dir.path().join("app-1.0-rebuilt.jar"), zip_raw(&[(class, &shifted), (item_class, &item)])).unwrap();
    fs::write(dir.path().join("app-1.1.jar"), zip_raw(&[(class, &v2), (item_class, &item)])).unwrap();
    let jar = |name: &str| dir.path().join(name).display().to_string();

    let rebuilt = statuses(&jar("app-1.0.jar"), &jar("app-1.0-rebuilt.jar"), &CompareOptions::default());
    assert_eq!(status_of(&rebuilt, class), Status::Similar);
    assert_eq!(status_of(&rebuilt, item_class), Status::Same);
    let changed = statuses(&jar("app-1.0.jar"), &jar("app-1.1.jar"), &CompareOptions::default());
    assert_eq!(status_of(&changed, class), Status::Different);

    let reason = parallax_core::convert::content_equal("OrderService.class", &v1, &shifted);
    assert_eq!(reason, Some("same code, only debug info or constant pool layout differ"));
    let without_rules = CompareOptions { compare_contents: false, ..CompareOptions::default() };
    let rebuilt = statuses(&jar("app-1.0.jar"), &jar("app-1.0-rebuilt.jar"), &without_rules);
    assert_eq!(status_of(&rebuilt, class), Status::Different);
}

#[test]
fn file_history_and_git_location() {
    let dir = make_repo();
    let repo = dir.path();
    write(repo, &[("src/cart.ts", "export const MAX = 75;\n")]);
    git(repo, &["commit", "-q", "-am", "raise the limit"]);
    let git_repo = parallax_core::vfs::git::GitRepo::discover(repo).unwrap();

    let log = git_repo.file_log("HEAD", "src/cart.ts", 50, 1000).unwrap();
    let summaries: Vec<&str> = log.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["raise the limit", "initial"]);
    // Commits on another branch count from there; the limit applies.
    let log = git_repo.file_log("feature", "src/cart.ts", 50, 1000).unwrap();
    assert_eq!(log.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(), ["feature work", "initial"]);
    assert_eq!(git_repo.file_log("HEAD", "src/cart.ts", 1, 1000).unwrap().len(), 1);
    assert!(git_repo.file_log("HEAD", "missing.txt", 50, 1000).unwrap().is_empty());

    // A file in the working tree is found in its repository.
    let local = SourceSpec::Local { path: repo.join("src/cart.ts").display().to_string() };
    let at = local.git_location().unwrap();
    assert_eq!(at.path, "src/cart.ts");
    assert_eq!(at.rev, None);
    assert_eq!(fs::canonicalize(&at.repo).unwrap(), fs::canonicalize(repo).unwrap());
    let rev = SourceSpec::parse(&format!("{}@feature:src/cart.ts", repo.display()));
    assert_eq!(rev.git_location().unwrap().rev.as_deref(), Some("feature"));

    // Branch names complete after `repo@`.
    let done = parallax_core::source::complete(&format!("{}@fe", repo.display()), 10);
    assert_eq!(done, [format!("{}@feature", repo.display())]);
}

fn pair(left: &Path, right: &Path) -> (Source, Source) {
    open_pair(
        &SourceSpec::parse(&left.display().to_string()),
        &SourceSpec::parse(&right.display().to_string()),
        SourceOptions::default(),
        false,
    )
    .unwrap()
}

/// The same file on both sides: one folder opened twice, or a hard link. Copying
/// it must not truncate it (large files are read through a memory map).
#[test]
fn copying_a_file_onto_itself_keeps_it() {
    let a = tempfile::tempdir().unwrap();
    let big = vec![b'A'; 512 * 1024];
    fs::write(a.path().join("big.bin"), &big).unwrap();
    fs::write(a.path().join("small.txt"), "small").unwrap();
    let (l, r) = pair(a.path(), a.path());
    copy(&l, &r, "big.bin").unwrap();
    copy(&l, &r, "small.txt").unwrap();
    assert_eq!(fs::read(a.path().join("big.bin")).unwrap(), big);
    assert_eq!(fs::read_to_string(a.path().join("small.txt")).unwrap(), "small");

    #[cfg(unix)]
    {
        let b = tempfile::tempdir().unwrap();
        fs::hard_link(a.path().join("big.bin"), b.path().join("big.bin")).unwrap();
        let (l, r) = pair(a.path(), b.path());
        copy(&l, &r, "big.bin").unwrap();
        assert_eq!(fs::read(a.path().join("big.bin")).unwrap(), big);
    }
}

/// A symlink on the destination side is replaced by the copied file; the file it
/// points to stays as it is.
#[cfg(unix)]
#[test]
fn copying_over_a_symlink_replaces_the_link() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(a.path().join("link"), "new content").unwrap();
    fs::write(outside.path().join("outside.txt"), "DO NOT OVERWRITE").unwrap();
    std::os::unix::fs::symlink(outside.path().join("outside.txt"), b.path().join("link")).unwrap();
    let (l, r) = pair(a.path(), b.path());
    assert_eq!(copy(&l, &r, "link").unwrap(), 1);
    assert_eq!(fs::read_to_string(outside.path().join("outside.txt")).unwrap(), "DO NOT OVERWRITE");
    assert!(!fs::symlink_metadata(b.path().join("link")).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(b.path().join("link")).unwrap(), "new content");
}

#[cfg(unix)]
#[test]
fn copying_keeps_the_executable_bit() {
    use std::os::unix::fs::PermissionsExt;
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write(a.path(), &[("tools/script.sh", "#!/bin/sh\n"), ("tools/notes.txt", "x")]);
    fs::set_permissions(a.path().join("tools/script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(a.path().join("tools/notes.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    let (l, r) = pair(a.path(), b.path());
    copy(&l, &r, "tools").unwrap();
    let mode = |p: &str| fs::metadata(b.path().join(p)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode("tools/script.sh"), 0o755);
    assert_eq!(mode("tools/notes.txt"), 0o640);
    // Over an existing file too.
    fs::write(a.path().join("tools/script.sh"), "#!/bin/sh\necho\n").unwrap();
    copy(&l, &r, "tools/script.sh").unwrap();
    assert_eq!(mode("tools/script.sh"), 0o755);
}

/// Two different payloads with the same CRC32 and size: by default the stored CRC32
/// settles it (fast, without decompressing); verifying compares the bytes, inside
/// zips and against a plain file.
#[test]
fn equal_crc32_with_and_without_verifying() {
    let a: &[u8] = &[0x13, 0x51, 0x7a, 0x08, 0xbf, 0xc8, 0xc2, 0x63];
    let b: &[u8] = &[0x5d, 0xfc, 0xf5, 0x23, 0x46, 0x54, 0x0e, 0xd9];
    assert_eq!(crc32fast::hash(a), crc32fast::hash(b));
    let dir = tempfile::tempdir().unwrap();
    let (left, right) = (dir.path().join("l.zip"), dir.path().join("r.zip"));
    fs::write(&left, zip_raw(&[("payload.bin", a)])).unwrap();
    fs::write(&right, zip_raw(&[("payload.bin", b)])).unwrap();
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("payload.bin"), b).unwrap();
    let (left, right, folder) = (left.display().to_string(), right.display().to_string(), folder.display().to_string());

    let trusting = CompareOptions::default();
    assert_eq!(status_of(&statuses(&left, &right, &trusting), "payload.bin"), Status::Same);

    let verifying = CompareOptions { verify_checksums: true, ..Default::default() };
    assert_eq!(status_of(&statuses(&left, &right, &verifying), "payload.bin"), Status::Different);
    assert_eq!(status_of(&statuses(&left, &folder, &verifying), "payload.bin"), Status::Different);
    // Equal data with an equal CRC is still the same.
    assert_eq!(status_of(&statuses(&right, &folder, &verifying), "payload.bin"), Status::Same);
}

#[cfg(unix)]
#[test]
fn a_symlink_is_not_a_file_with_its_target_as_content() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("real.txt", a.path().join("link")).unwrap();
    fs::write(b.path().join("link"), "real.txt").unwrap();
    let list = statuses(&a.path().display().to_string(), &b.path().display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "link"), Status::Different);
}

/// A symlink copied into a 7z archive is read back as a symlink and copied out as one.
#[cfg(unix)]
#[test]
fn symlinks_survive_a_round_trip_through_7z() {
    for kind in ["7z", "7z-non-solid"] {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("pack.7z");
        fs::write(&archive, archive_bytes(kind, &[("keep.txt", "k")])).unwrap();
        let src = dir.path().join("src");
        write(&src, &[("real.txt", "real")]);
        std::os::unix::fs::symlink("real.txt", src.join("link")).unwrap();
        std::os::unix::fs::symlink("missing", src.join("dangling")).unwrap();
        let (s, z) = pair(&src, &archive);
        copy(&s, &z, "link").unwrap();
        copy(&s, &z, "dangling").unwrap();
        let (_, z) = pair(&src, &archive);
        assert_eq!(z.stat("link").unwrap().kind, EntryKind::Symlink, "{kind}");
        assert_eq!(z.stat("keep.txt").unwrap().kind, EntryKind::File, "{kind}");
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let (z, o) = pair(&archive, &out);
        copy(&z, &o, "link").unwrap();
        copy(&z, &o, "dangling").unwrap();
        assert_eq!(fs::read_link(out.join("link")).unwrap(), Path::new("real.txt"), "{kind}");
        assert_eq!(fs::read_link(out.join("dangling")).unwrap(), Path::new("missing"), "{kind}");
    }
}

/// A zip with data in front of it, like a self-extracting archive (`cat stub zip`):
/// opened by detecting where the archive starts.
#[test]
fn zips_with_data_in_front_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut data = vec![b'M'; 4096];
    data.extend(zip_bytes(&[("a/x.txt", "x"), ("y.txt", "y")]));
    fs::write(dir.path().join("sfx.zip"), data).unwrap();
    fs::write(dir.path().join("plain.zip"), zip_bytes(&[("a/x.txt", "x"), ("y.txt", "y")])).unwrap();
    let list = statuses(
        &dir.path().join("sfx.zip").display().to_string(),
        &dir.path().join("plain.zip").display().to_string(),
        &CompareOptions::default(),
    );
    assert_eq!(status_of(&list, "a/x.txt"), Status::Same, "{list:?}");
    assert_eq!(status_of(&list, "y.txt"), Status::Same, "{list:?}");
}

/// Entries inside zips are compared as streams: differences, line endings and
/// equal entries with different CRC32 (stored vs re-encoded) come out as before.
#[test]
fn zip_entries_compare_as_streams() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
    let crlf = body.replace('\n', "\r\n");
    let mut changed_late = body.clone();
    changed_late.push_str("one more\n");
    let left = zip_bytes(&[("lf.txt", &body), ("late.txt", &body), ("same.txt", &body)]);
    let right = zip_bytes(&[("lf.txt", &crlf), ("late.txt", &changed_late), ("same.txt", &body)]);
    fs::write(dir.path().join("l.zip"), left).unwrap();
    fs::write(dir.path().join("r.zip"), right).unwrap();
    let (l, r) = (dir.path().join("l.zip").display().to_string(), dir.path().join("r.zip").display().to_string());
    let list = statuses(&l, &r, &CompareOptions::default());
    assert_eq!(status_of(&list, "lf.txt"), Status::Similar);
    assert_eq!(status_of(&list, "late.txt"), Status::Different);
    assert_eq!(status_of(&list, "same.txt"), Status::Same);
    let strict = CompareOptions { ignore_line_endings: false, ..Default::default() };
    assert_eq!(status_of(&statuses(&l, &r, &strict), "lf.txt"), Status::Different);
    // Against a folder: the plain file on one side, the zip entry streamed on the other.
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("lf.txt"), &crlf).unwrap();
    fs::write(folder.join("same.txt"), &body).unwrap();
    let list = statuses(&l, &folder.display().to_string(), &CompareOptions::default());
    assert_eq!(status_of(&list, "lf.txt"), Status::Similar);
    assert_eq!(status_of(&list, "same.txt"), Status::Same);
}
