//! Real filesystem exhaustion checks. The launcher supplies a disposable, bounded
//! volume and points TEMP/TMP at it. Never run against a user's normal drive.
use parallax_core::{
    progress::Progress,
    vfs::{
        Change, Data, EntryKind, Vfs,
        archive::{ArchiveFormat, ArchiveFs},
        local::LocalFs,
        mount::ArchiveMount,
    },
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
fn put(path: &str, data: &[u8]) -> Change {
    Change::Put { path: path.into(), kind: EntryKind::File, data: Data::from_vec(data.to_vec()), mtime: None }
}
fn zip() -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file("value.txt", zip::write::SimpleFileOptions::default()).unwrap();
    z.write_all(b"original archive content").unwrap();
    z.finish().unwrap().into_inner()
}
struct Fill(PathBuf);
impl Drop for Fill {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn exhaust(root: &Path) -> Fill {
    let path = root.join("filler.bin");
    let mut file = fs::File::create(&path).unwrap();
    let block = [19u8; 64 * 1024];
    let mut written = 0;
    let error = loop {
        match file.write_all(&block) {
            Ok(()) => {
                written += block.len();
                assert!(written <= 128 * 1024 * 1024, "test volume is not bounded to 128 MiB");
            }
            Err(e) => break e,
        }
    };
    assert!(
        error.kind() == std::io::ErrorKind::StorageFull || matches!(error.raw_os_error(), Some(39 | 112)),
        "unexpected filler failure: {error}"
    );
    // Consume the remainder too: one failed 64 KiB allocation can leave enough
    // space for a small temporary file, depending on the filesystem.
    while file.write_all(&[19; 512]).is_ok() {}
    drop(file);
    Fill(path)
}

#[test]
#[ignore = "requires the disposable bounded volume created by windows-disk-full.ps1"]
fn real_disk_full_extraction_and_saves() {
    let root = PathBuf::from(std::env::var_os("PARALLAX_FULL_VOLUME").expect("disposable volume path"));
    assert_eq!(
        fs::read_to_string(root.join("parallax-pressure-volume.marker")).unwrap(),
        "disposable-parallax-test-volume\n"
    );
    assert!(std::env::temp_dir().starts_with(&root), "TEMP/TMP must point at the disposable volume");
    let work = PathBuf::from(std::env::var_os("PARALLAX_PRESSURE_WORK").expect("work directory on another volume"));
    assert!(!work.starts_with(&root));
    fs::create_dir_all(&work).unwrap();
    let local = Arc::new(LocalFs::new(&root));
    fs::write(root.join("value.arxml"), b"<AUTOSAR>original</AUTOSAR>").unwrap();
    let original_archive = zip();
    fs::write(root.join("pack.zip"), &original_archive).unwrap();
    let mount = ArchiveMount::new(local.clone(), true);
    assert_eq!(&*mount.read("pack.zip/value.txt").unwrap(), b"original archive content");
    let small = work.join("small-source");
    fs::create_dir_all(&small).unwrap();
    fs::write(small.join("value.txt"), b"original 7z content").unwrap();
    let mut writer = sevenz_rust2::ArchiveWriter::create(root.join("pack.7z")).unwrap();
    writer.push_source_path(&small, |_| true).unwrap();
    writer.finish().unwrap();
    let original_7z = fs::read(root.join("pack.7z")).unwrap();
    assert_eq!(&*mount.read("pack.7z/value.txt").unwrap(), b"original 7z content");
    let src = work.join("solid-source");
    fs::create_dir_all(&src).unwrap();
    for i in 0..2 {
        fs::File::create(src.join(format!("{i}.bin"))).unwrap().set_len(80 << 20).unwrap();
    }
    let archive_path = work.join("spill.7z");
    let mut writer = sevenz_rust2::ArchiveWriter::create(&archive_path).unwrap();
    writer.set_content_methods(vec![sevenz_rust2::EncoderConfiguration::new(sevenz_rust2::EncoderMethod::COPY)]);
    writer.push_source_path(&src, |_| true).unwrap();
    writer.finish().unwrap();
    let archive = ArchiveFs::open_file(&archive_path, ArchiveFormat::SevenZ, "spill.7z").unwrap();
    // Repeated bytes compress into a resident NTFS record even on a full drive.
    // Incompressible output must allocate data clusters in every archive format.
    let mut state = 0x1234_5678_9abc_def0u64;
    let replacement: Vec<u8> = (0..1 << 20)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 32) as u8
        })
        .collect();
    let filler = exhaust(&root);
    let error = local.apply(vec![put("value.arxml", &replacement)], &Progress::default()).unwrap_err().to_string();
    println!("local save error: {error}");
    assert!(error.contains("full") && error.contains("value.arxml"), "{error}");
    assert_eq!(fs::read(root.join("value.arxml")).unwrap(), b"<AUTOSAR>original</AUTOSAR>");
    let error =
        mount.apply(vec![put("pack.zip/value.txt", &replacement)], &Progress::default()).unwrap_err().to_string();
    println!("archive save error: {error}");
    assert!(error.contains("full") && error.contains("pack.zip"), "{error}");
    assert_eq!(fs::read(root.join("pack.zip")).unwrap(), original_archive);
    let error =
        mount.apply(vec![put("pack.7z/value.txt", &replacement)], &Progress::default()).unwrap_err().to_string();
    println!("7z save error: {error}");
    assert!(error.contains("full") && error.contains("pack.7z"), "{error}");
    assert_eq!(fs::read(root.join("pack.7z")).unwrap(), original_7z);
    assert!(
        !fs::read_dir(&root).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".parallax-tmp")),
        "failed saves must remove their temporary files"
    );
    let error = (0..2)
        .find_map(|i| archive.read(&format!("{i}.bin")).err())
        .expect("solid decoding must exhaust its memory budget and spill to the full temp volume")
        .to_string();
    println!("7z extraction error: {error}");
    assert!(
        error.contains("Temporary drive is full")
            && error.contains("spill.7z")
            && error.contains(root.to_str().unwrap()),
        "{error}"
    );
    drop(filler);
    local.apply(vec![put("value.arxml", b"<AUTOSAR>saved</AUTOSAR>")], &Progress::default()).unwrap();
    mount.apply(vec![put("pack.zip/value.txt", b"saved archive content")], &Progress::default()).unwrap();
    assert_eq!(&*mount.read("pack.zip/value.txt").unwrap(), b"saved archive content");
    mount.apply(vec![put("pack.7z/value.txt", b"saved 7z content")], &Progress::default()).unwrap();
    assert_eq!(&*mount.read("pack.7z/value.txt").unwrap(), b"saved 7z content");
    // Retry on the SAME archive handle: failed cached state must not poison it.
    for i in 0..2 {
        let data = archive.read(&format!("{i}.bin")).unwrap();
        assert_eq!(data.len(), 80 << 20);
        assert!(data.iter().all(|b| *b == 0));
    }
    assert_eq!(fs::read(root.join("value.arxml")).unwrap(), b"<AUTOSAR>saved</AUTOSAR>");
    println!(
        "PASS: local and archive originals preserved, temporary files cleaned, all retries succeeded without reopening the 7z archive"
    );
}
