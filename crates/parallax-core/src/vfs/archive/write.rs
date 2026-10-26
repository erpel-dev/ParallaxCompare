//! Writing to archives. A batch of changes rewrites the archive once, keeping what
//! it can of the old one as stored:
//!
//! * zip: unchanged entries are copied compressed; new ones are deflated.
//! * 7z: unchanged blocks are copied encoded; a block that loses a file is decoded
//!   and the rest of it encoded again (for a solid archive, that can be all of it);
//!   new files go into one new block (non-solid archives: one block each).
//! * tar: the stream is rebuilt and compressed again.

use std::collections::HashSet;
use std::io::{self, Cursor, Write};
use std::num::NonZeroU64;
use std::time::{Duration, SystemTime};

use super::{ArchiveData, ArchiveFormat, ArchiveFs, ArchiveReader, Backend, civil_to_unix, tar_reader};
use crate::progress::Progress;
use crate::vfs::{Change, Data, EntryKind, Vfs, normalize};
use crate::{Error, Result};

/// LZMA2 and xz level for new data; 7-Zip's default.
const LZMA_LEVEL: u32 = 5;

impl ArchiveFs {
    /// The archive with `changes` applied, in the same format. Paths are relative to
    /// the archive root and must not enter archives nested in it.
    /// `progress` counts entries; cancelling stops before the new archive is complete.
    pub fn rewritten(&self, changes: Vec<Change>, progress: &Progress) -> Result<Vec<u8>> {
        let edits = Edits::new(changes);
        // A file where a folder is would leave the folder's entries beside it: an
        // archive whose path is both, which tools cannot extract.
        for put in edits.puts.iter().filter(|p| p.kind != EntryKind::Dir) {
            let removed_first = edits.removed.iter().any(|r| is_within(&put.path, r));
            if !removed_first && self.stat(&put.path).is_ok_and(|e| e.kind == EntryKind::Dir) {
                return Err(Error::Unsupported(format!(
                    "{} is a folder in {}; a file cannot replace it",
                    put.path,
                    self.label()
                )));
            }
        }
        progress.expect(edits.puts.len() as u64);
        match &self.backend {
            Backend::Zip { archive, .. } => write_zip(archive, &edits, progress),
            Backend::SevenZ { data, archive, .. } => write_7z(data, archive, &edits, progress),
            Backend::Tar { data, format, contents, .. } => {
                progress.expect(contents.len() as u64);
                write_tar(data, *format, &edits, progress)
            }
        }
    }
}

/// Changes to one archive: what is added and which old entries are left out.
struct Edits {
    puts: Vec<Put>,
    /// Removed paths; everything below them goes too.
    removed: Vec<String>,
    /// Paths of the puts: an old entry of the same name is replaced.
    replaced: HashSet<String>,
}

struct Put {
    path: String,
    kind: EntryKind,
    data: Data,
    mtime: Option<i64>,
}

impl Edits {
    fn new(changes: Vec<Change>) -> Self {
        let mut edits = Edits { puts: Vec::new(), removed: Vec::new(), replaced: HashSet::new() };
        for change in changes {
            match change {
                Change::Put { path, kind, data, mtime } => {
                    if !edits.replaced.insert(path.clone()) {
                        edits.puts.retain(|p| p.path != path);
                    }
                    edits.puts.push(Put { path, kind, data, mtime });
                }
                Change::Remove { path } => {
                    edits.puts.retain(|p| !is_within(&p.path, &path));
                    edits.removed.push(path);
                }
            }
        }
        edits
    }

    /// Whether the old entry named `name` (as stored) is left out.
    fn drops(&self, name: &str) -> bool {
        // Entries whose names cannot be shown are kept as they are.
        let Some(path) = normalize(name).filter(|p| !p.is_empty()) else {
            return false;
        };
        self.replaced.contains(&path) || self.removed.iter().any(|r| is_within(&path, r))
    }
}

fn is_within(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn threads() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

fn write_zip(archive: &zip::ZipArchive<ArchiveReader>, edits: &Edits, progress: &Progress) -> Result<Vec<u8>> {
    let mut old = archive.clone();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    progress.expect(old.len() as u64);
    for i in 0..old.len() {
        progress.advance(1)?;
        let file = old.by_index_raw(i).map_err(Error::archive)?;
        if !file.name().is_ok_and(|n| edits.drops(&n)) {
            zip.raw_copy_file(file).map_err(Error::archive)?;
        }
    }
    for put in &edits.puts {
        progress.advance(1)?;
        let mut options = zip::write::SimpleFileOptions::default().large_file(put.data.len() as u64 >= u32::MAX as u64);
        if let Some(time) = put.mtime.and_then(zip_time) {
            options = options.last_modified_time(time);
        }
        match put.kind {
            EntryKind::Dir => zip.add_directory(&put.path, options),
            EntryKind::Symlink => zip.add_symlink(&put.path, String::from_utf8_lossy(&put.data), options),
            _ => zip.start_file(&put.path, options).and_then(|()| Ok(zip.write_all(&put.data)?)),
        }
        .map_err(Error::archive)?;
    }
    if !old.comment().is_empty() {
        zip.set_raw_comment(old.comment().into()).map_err(Error::archive)?;
    }
    Ok(zip.finish().map_err(Error::archive)?.into_inner())
}

/// Zip times have no time zone; like the reader, this writes UTC.
fn zip_time(secs: i64) -> Option<zip::DateTime> {
    let (y, m, d, hh, mm, ss) = unix_to_civil(secs);
    zip::DateTime::from_date_and_time(y.try_into().ok()?, m as u8, d as u8, hh as u8, mm as u8, ss as u8).ok()
}

/// Inverse of [`civil_to_unix`]: (year, month, day, hour, minute, second) in UTC.
fn unix_to_civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    debug_assert_eq!(civil_to_unix(y, m as i64, d as i64) + rest as i64, secs);
    (y, m, d, rest / 3600, rest % 3600 / 60, rest % 60)
}

fn write_7z(
    data: &ArchiveData,
    archive: &sevenz_rust2::Archive,
    edits: &Edits,
    progress: &Progress,
) -> Result<Vec<u8>> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, BlockDecoder, Password, SourceReader};

    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).map_err(Error::archive)?;
    writer.set_content_methods(vec![
        sevenz_rust2::encoder_options::Lzma2Options::from_level_mt(LZMA_LEVEL, threads(), 4 << 20).into(),
    ]);
    let dropped = |entry: &ArchiveEntry| edits.drops(&entry.name);
    let block_of = &archive.stream_map.file_block_index;
    let mut changed = vec![false; archive.blocks.len()];
    let mut files_in = vec![0u64; archive.blocks.len()];
    for (file, block) in archive.files.iter().zip(block_of) {
        if let Some(block) = block {
            files_in[*block] += 1;
            changed[*block] |= dropped(file);
        }
    }
    progress.expect(files_in.iter().sum());

    let password = Password::empty();
    let mut source = data.reader();
    for (block, &changed) in changed.iter().enumerate() {
        if !changed {
            progress.advance(files_in[block])?;
            writer.push_raw_block(archive, block, &mut source, |e| e.name.clone()).map_err(Error::archive)?;
            continue;
        }
        let mut kept = Vec::new();
        let mut readers = Vec::new();
        BlockDecoder::new(threads(), block, archive, &password, &mut source)
            .for_each_entries(&mut |entry, r| {
                if progress.advance(1).is_err() {
                    return Ok(false);
                }
                if dropped(entry) {
                    // Read past it: the next file's data follows in the same stream.
                    io::copy(r, &mut io::sink())?;
                } else {
                    // A header cannot make this allocation large; see `entry_hint`.
                    let hint = super::entry_hint(entry.size).map_err(std::io::Error::other)?;
                    let mut buf = Vec::with_capacity(hint);
                    r.read_to_end(&mut buf)?;
                    kept.push(entry.clone());
                    readers.push(SourceReader::new(Counted::new(Cursor::new(buf), progress)));
                }
                Ok(true)
            })
            .map_err(Error::archive)?;
        progress.check()?;
        if !kept.is_empty() {
            // Encoding is the slow part: it is counted and can be cancelled per file.
            progress.expect(kept.len() as u64);
            writer.push_archive_entries(kept, readers).map_err(|e| cancelled_or(progress, e))?;
        }
    }
    // Folders and empty files belong to no block. They are written folders first:
    // sevenz-rust2 sizes the empty-file bit vector by the last empty file, so a folder
    // after it would fall outside the vector and 7-Zip rejects the header.
    let mut no_data: Vec<ArchiveEntry> = archive
        .files
        .iter()
        .zip(block_of)
        .filter(|(file, block)| block.is_none() && !file.is_anti_item && !dropped(file))
        .map(|(file, _)| file.clone())
        .collect();
    let mut entries = Vec::new();
    let mut readers = Vec::new();
    for put in &edits.puts {
        progress.advance(1)?;
        let mut entry = match put.kind {
            EntryKind::Dir => ArchiveEntry::new_directory(&put.path),
            _ => ArchiveEntry::new_file(&put.path),
        };
        if let Some(time) = put.mtime.filter(|t| *t >= 0) {
            let time = SystemTime::UNIX_EPOCH + Duration::from_secs(time as u64);
            if let Ok(time) = time.try_into() {
                entry.has_last_modified_date = true;
                entry.last_modified_date = time;
            }
        }
        if put.kind == EntryKind::Symlink {
            // As 7-Zip stores a Unix symlink: the Unix mode in the high 16 attribute bits.
            entry.has_windows_attributes = true;
            entry.windows_attributes = 0x8000 | (0o120777 << 16);
        }
        if put.kind == EntryKind::Dir || put.data.is_empty() {
            entry.has_stream = false;
            no_data.push(entry);
        } else if archive.is_solid {
            entries.push(entry);
            readers.push(SourceReader::new(Counted::new(Cursor::new(put.data.clone()), progress)));
        } else {
            writer.push_archive_entry(entry, Some(Cursor::new(put.data.clone()))).map_err(Error::archive)?;
        }
    }
    no_data.sort_by_key(|e| !e.is_directory);
    for entry in no_data {
        writer.push_archive_entry(entry, None::<&[u8]>).map_err(Error::archive)?;
    }
    if !entries.is_empty() {
        progress.expect(entries.len() as u64);
        writer.push_archive_entries(entries, readers).map_err(|e| cancelled_or(progress, e))?;
    }
    Ok(writer.finish().map_err(Error::archive)?.into_inner())
}

/// Reads a file's data for the encoder, counting the file once it is read and
/// stopping the encoder when the operation is cancelled.
struct Counted<'a, R> {
    inner: R,
    progress: &'a Progress,
    counted: bool,
}

impl<'a, R> Counted<'a, R> {
    fn new(inner: R, progress: &'a Progress) -> Self {
        Counted { inner, progress, counted: false }
    }
}

impl<R: io::Read> io::Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.progress.check().is_err() {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        if n == 0 && !buf.is_empty() && !self.counted {
            self.counted = true;
            self.progress.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(n)
    }
}

fn cancelled_or(progress: &Progress, e: impl std::fmt::Display) -> Error {
    progress.check().err().unwrap_or_else(|| Error::archive(e))
}

fn write_tar(data: &ArchiveData, format: ArchiveFormat, edits: &Edits, progress: &Progress) -> Result<Vec<u8>> {
    let mut old = tar::Archive::new(tar_reader(data, format)?);
    let mut tar = tar::Builder::new(Vec::new());
    for entry in old.entries().map_err(Error::archive)? {
        progress.advance(1)?;
        let mut entry = entry.map_err(Error::archive)?;
        let path = entry.path().map_err(Error::archive)?.into_owned();
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions() || edits.drops(&path.to_string_lossy()) {
            continue;
        }
        let mut header = entry.header().clone();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            let target = entry.link_name().map_err(Error::archive)?.unwrap_or_default().into_owned();
            tar.append_link(&mut header, &path, &target)
        } else {
            if entry_type.is_gnu_sparse() {
                // Reading fills in the holes, so the copy is a plain file.
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(entry.size());
            }
            tar.append_data(&mut header, &path, &mut entry)
        }
        .map_err(Error::archive)?;
    }

    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for put in &edits.puts {
        progress.advance(1)?;
        let mut header = tar::Header::new_gnu();
        header.set_mtime(put.mtime.map_or(now, |t| t.max(0) as u64));
        match put.kind {
            EntryKind::Dir => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(0o755);
                header.set_size(0);
                tar.append_data(&mut header, &put.path, io::empty())
            }
            EntryKind::Symlink => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_mode(0o777);
                header.set_size(0);
                tar.append_link(&mut header, &put.path, String::from_utf8_lossy(&put.data).as_ref())
            }
            _ => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode(0o644);
                header.set_size(put.data.len() as u64);
                tar.append_data(&mut header, &put.path, &*put.data)
            }
        }
        .map_err(Error::archive)?;
    }
    let tar = tar.into_inner().map_err(Error::archive)?;
    progress.check()?;
    compress(tar, format).map_err(Error::archive)
}

fn compress(tar: Vec<u8>, format: ArchiveFormat) -> io::Result<Vec<u8>> {
    Ok(match format {
        ArchiveFormat::Tar => tar,
        ArchiveFormat::TarGz => {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&tar)?;
            encoder.finish()?
        }
        ArchiveFormat::TarBz2 => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            encoder.write_all(&tar)?;
            encoder.finish()?
        }
        ArchiveFormat::TarXz => {
            let mut options = lzma_rust2::XzOptions::with_preset(LZMA_LEVEL);
            // Blocks compress in parallel; smaller ones compress a little worse.
            let block = (tar.len() as u64 / u64::from(threads())).clamp(1 << 20, 24 << 20);
            options.set_block_size(NonZeroU64::new(block));
            let mut encoder = lzma_rust2::XzWriterMt::new(Vec::new(), options, threads())?;
            encoder.write_all(&tar)?;
            encoder.finish()?
        }
        ArchiveFormat::TarZst => {
            ruzstd::encoding::compress_to_vec(&tar[..], ruzstd::encoding::CompressionLevel::Fastest)
        }
        ArchiveFormat::Zip | ArchiveFormat::SevenZ => unreachable!("not a tar format"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for secs in [0, 951_868_800, 1_700_000_000 + 3723, -86_401, 4_102_444_800] {
            let (y, m, d, hh, mm, ss) = unix_to_civil(secs);
            let back = civil_to_unix(y, m as i64, d as i64) + (hh * 3600 + mm * 60 + ss) as i64;
            assert_eq!(back, secs);
        }
        assert_eq!(unix_to_civil(951_868_800), (2000, 3, 1, 0, 0, 0));
    }

    /// A file cannot replace a folder; removing the folder in the same batch first works.
    #[test]
    fn files_do_not_replace_folders() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("node/child.txt", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"child").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let archive = ArchiveFs::open(Data::from_vec(bytes), ArchiveFormat::Zip, "t.zip").unwrap();
        let put = || Change::Put {
            path: "node".into(),
            kind: EntryKind::File,
            data: Data::from_vec(b"file".to_vec()),
            mtime: None,
        };
        let err = archive.rewritten(vec![put()], &Progress::default()).unwrap_err().to_string();
        assert!(err.contains("is a folder"), "{err}");
        let data =
            archive.rewritten(vec![Change::Remove { path: "node".into() }, put()], &Progress::default()).unwrap();
        let again = ArchiveFs::open(Data::from_vec(data), ArchiveFormat::Zip, "t.zip").unwrap();
        assert_eq!(again.stat("node").unwrap().kind, EntryKind::File);
        assert!(again.stat("node/child.txt").is_err());
    }

    #[test]
    fn edits_drop_replaced_and_removed_paths() {
        let put =
            |p: &str| Change::Put { path: p.into(), kind: EntryKind::File, data: Data::from_vec(vec![]), mtime: None };
        let edits = Edits::new(vec![put("a/b.txt"), Change::Remove { path: "old".into() }, put("old/x")]);
        assert!(edits.drops("a/b.txt"));
        assert!(edits.drops("./a\\b.txt"));
        assert!(edits.drops("old/deep/file"));
        assert!(!edits.drops("older"));
        assert!(!edits.drops("a"));
        assert_eq!(edits.puts.len(), 2);
    }
}
