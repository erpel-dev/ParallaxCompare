//! Archives as read-only trees: zip (and jar, nupkg, …), 7z and tar with optional compression.
//!
//! The directory index is built once from the archive headers, so listing never
//! decompresses anything. zip and 7z headers carry a CRC32 per entry, which the
//! compare engine uses as a [`QuickId`].
//!
//! Writing rewrites the whole archive, see [`write`].

mod write;

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::sync::Arc;

use parking_lot::Mutex;

use super::{Data, Entry, EntryKind, QuickId, Vfs, check_rel, find_entry, join, normalize, root_entry, split_parent};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
}

/// Detects an archive by file name. Zip-based package formats are included.
pub fn detect_format(name: &str) -> Option<ArchiveFormat> {
    let lower = name.to_ascii_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| lower.ends_with(e));
    Some(if ends(&[".zip", ".jar", ".war", ".ear", ".apk", ".nupkg", ".vsix", ".whl", ".aar", ".xpi", ".pdx"]) {
        ArchiveFormat::Zip
    } else if ends(&[".7z"]) {
        ArchiveFormat::SevenZ
    } else if ends(&[".tar"]) {
        ArchiveFormat::Tar
    } else if ends(&[".tar.gz", ".tgz", ".crate"]) {
        ArchiveFormat::TarGz
    } else if ends(&[".tar.xz", ".txz"]) {
        ArchiveFormat::TarXz
    } else if ends(&[".tar.zst", ".tzst"]) {
        ArchiveFormat::TarZst
    } else if ends(&[".tar.bz2", ".tbz2", ".tbz"]) {
        ArchiveFormat::TarBz2
    } else {
        return None;
    })
}

/// Directory index shared by all formats.
#[derive(Default)]
struct Index {
    dirs: HashMap<String, Vec<Entry>>,
    known_dirs: HashSet<String>,
    /// Where each file sits in its folder's list, to replace a repeated entry
    /// without searching the folder (which made large flat archives quadratic).
    file_at: HashMap<String, usize>,
}

impl Index {
    fn new() -> Self {
        let mut index = Index::default();
        index.dirs.insert(String::new(), Vec::new());
        index.known_dirs.insert(String::new());
        index
    }

    /// Creates `path` and every missing parent, from the root down. Iterative, so
    /// that a member name with thousands of components cannot overflow the stack;
    /// names deeper than [`MAX_DEPTH`] are not listed.
    fn ensure_dir(&mut self, path: &str, mtime: Option<i64>) {
        if self.known_dirs.contains(path) {
            return;
        }
        let mut prefix = String::new();
        for (depth, part) in path.split('/').enumerate() {
            if depth >= MAX_DEPTH {
                return;
            }
            let dir = join(&prefix, part);
            if !self.known_dirs.contains(&dir) {
                let (parent, name) = split_parent(&dir);
                self.known_dirs.insert(dir.clone());
                self.dirs.entry(dir.clone()).or_default();
                // The parents do not inherit the mtime of the entry that needed them.
                let entry = Entry {
                    name: name.to_string(),
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime: if dir == path { mtime } else { None },
                    quick_id: None,
                };
                self.dirs.get_mut(parent).expect("parent dir exists").push(entry);
            }
            prefix = dir;
        }
    }

    fn add_file(&mut self, path: &str, kind: EntryKind, size: u64, mtime: Option<i64>, quick_id: Option<QuickId>) {
        let (parent, name) = split_parent(path);
        self.ensure_dir(parent, None);
        if !self.known_dirs.contains(parent) {
            // Deeper than MAX_DEPTH: not listed, and nothing to add it to.
            return;
        }
        let list = self.dirs.get_mut(parent).expect("parent dir exists");
        let entry = Entry { name: name.to_string(), kind, size, mtime, quick_id };
        // Later entries win, like extraction would behave.
        if let Some(&at) = self.file_at.get(path) {
            list[at] = entry;
        } else if let Some(at) =
            self.known_dirs.contains(path).then(|| list.iter().position(|e| e.name == name)).flatten()
        {
            // A folder and a file of the same name: the file takes the folder's place.
            list[at] = entry;
            self.file_at.insert(path.to_string(), at);
        } else {
            self.file_at.insert(path.to_string(), list.len());
            list.push(entry);
        }
    }
}

type Extracted = Arc<HashMap<String, Arc<Vec<u8>>>>;

/// Blocks at least this large are decoded with several threads, when the
/// encoder split them into independent chunks (7-Zip does for LZMA2).
const MT_BLOCK_SIZE: u64 = 16 << 20;

/// No entry of an archive is expanded beyond this. The size in a header is
/// attacker-controlled, and neither memory nor a temporary file should follow it.
const MAX_ENTRY: u64 = 2 << 30;

/// No 7z block is decoded beyond this, since a solid block is held whole.
const MAX_BLOCK: u64 = 4 << 30;

/// How much is allocated up front: the rest is grown by reading, so that a crafted
/// header cannot make the process ask for gigabytes before a byte is read.
const READ_HINT: usize = 8 << 20;

/// A compressed tar may expand to this multiple of its own size, within these
/// bounds, before it is refused rather than written to the temporary file.
const TAR_RATIO: u64 = 200;
const TAR_MIN: u64 = 256 << 20;
const TAR_MAX: u64 = 4 << 30;

/// Directories that one archive member may create. Member names are unbounded, and
/// creating the parents recursively would overflow the stack on a crafted one.
const MAX_DEPTH: usize = 512;

/// Refuses an entry bigger than [`MAX_ENTRY`] and returns the allocation hint.
pub(super) fn entry_hint(size: u64) -> Result<usize> {
    if size > MAX_ENTRY {
        return Err(Error::Unsupported(format!(
            "an entry of this archive expands to {} MiB, more than a comparison reads",
            size >> 20
        )));
    }
    Ok(size.min(READ_HINT as u64) as usize)
}

/// Reads the `size` bytes of an entry whose header says so.
pub(super) fn read_entry(r: &mut dyn Read, size: u64) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(entry_hint(size)?);
    r.take(size).read_to_end(&mut buf).map_err(Error::archive)?;
    Ok(buf)
}

/// A range of bytes: of the archive itself for an uncompressed tar, of the
/// temporary file that holds the decompressed stream for a compressed one.
#[derive(Debug, Clone, Copy)]
struct Span {
    at: u64,
    len: u64,
}

/// Where an entry of a tar archive is.
enum TarContent {
    /// Inside the uncompressed archive, which stays memory-mapped.
    Direct(Span),
    /// Inside the decompressed stream, in the temporary file.
    Spooled(Span),
    /// Bytes that are not in the stream, such as a link's target path.
    Bytes(Arc<Vec<u8>>),
}

impl TarContent {
    fn len(&self) -> u64 {
        match self {
            TarContent::Direct(span) | TarContent::Spooled(span) => span.len,
            TarContent::Bytes(bytes) => bytes.len() as u64,
        }
    }
}

/// The decompressed stream of a compressed tar, in a temporary file: a tar holds
/// every entry, so keeping them in memory would let one archive take the machine.
struct Spool {
    file: Mutex<std::fs::File>,
    len: u64,
}

impl Spool {
    fn new() -> Result<Self> {
        let file = tempfile::tempfile().map_err(|e| Error::io("a temporary file for the tar stream", e))?;
        Ok(Spool { file: Mutex::new(file), len: 0 })
    }

    /// Appends `size` bytes of `r`, which the caller has already counted.
    fn append(&mut self, r: &mut dyn Read, size: u64) -> Result<Span> {
        use std::io::{Seek, SeekFrom};
        let err = |e| Error::io("a temporary file for the tar stream", e);
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(self.len)).map_err(err)?;
        let at = self.len;
        let copied = std::io::copy(&mut r.take(size), &mut *file).map_err(err)?;
        self.len += copied;
        Ok(Span { at, len: copied })
    }

    fn read(&self, span: Span) -> Result<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let err = |e| Error::io("a temporary file for the tar stream", e);
        let mut buf = Vec::with_capacity(span.len.min(READ_HINT as u64) as usize);
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(span.at)).map_err(err)?;
        file.by_ref().take(span.len).read_to_end(&mut buf).map_err(err)?;
        if buf.len() as u64 != span.len {
            return Err(Error::Unsupported("the tar archive is truncated".into()));
        }
        Ok(buf)
    }
}

/// The bytes of a tar entry.
fn tar_read(data: &Data, spool: Option<&Spool>, content: &TarContent) -> Result<Vec<u8>> {
    let slice = |span: &Span| -> Result<Vec<u8>> {
        let (at, len) = (usize::try_from(span.at), usize::try_from(span.len));
        match (at, len) {
            (Ok(at), Ok(len)) => data.get(at..at.saturating_add(len)).map(<[u8]>::to_vec),
            _ => None,
        }
        .ok_or_else(|| Error::Unsupported("the tar archive is truncated".into()))
    };
    match content {
        TarContent::Direct(span) => slice(span),
        TarContent::Spooled(span) => {
            spool.ok_or_else(|| Error::Unsupported("the tar stream is missing".into()))?.read(*span)
        }
        TarContent::Bytes(bytes) => Ok(bytes.to_vec()),
    }
}

enum Backend {
    Zip {
        archive: zip::ZipArchive<Cursor<Data>>,
        index_of: HashMap<String, usize>,
    },
    /// 7z is decoded per block. A solid block holds many files that can only be
    /// decompressed in sequence, so it is decoded completely on first read and kept;
    /// blocks of different files decode in parallel. A block with a single file
    /// (non-solid archives) is decoded on each read, like a zip entry.
    SevenZ {
        data: Data,
        archive: Box<sevenz_rust2::Archive>,
        index_of: HashMap<String, usize>,
        /// Per block: the decoded files of a solid block, `None` for a single-file block.
        solid: Vec<Option<Mutex<Option<Extracted>>>>,
    },
    /// tar streams are decompressed once while indexing; the decompressed stream
    /// is kept out of memory (see [`TarContent`]).
    Tar {
        data: Data,
        format: ArchiveFormat,
        contents: HashMap<String, TarContent>,
        /// The decompressed stream of a compressed tar.
        spool: Option<Spool>,
    },
}

pub struct ArchiveFs {
    label: String,
    index: Index,
    backend: Backend,
}

impl ArchiveFs {
    pub fn open(data: Data, format: ArchiveFormat, label: impl Into<String>) -> Result<Self> {
        let label = label.into();
        let mut index = Index::new();
        let backend = match format {
            ArchiveFormat::Zip => open_zip(data, &mut index)?,
            ArchiveFormat::SevenZ => open_7z(data, &mut index)?,
            _ => open_tar(data, format, &mut index)?,
        };
        Ok(ArchiveFs { label, index, backend })
    }
}

fn open_zip(data: Data, index: &mut Index) -> Result<Backend> {
    // Offset 0 first: detecting it scans the whole file for central directory
    // headers, which reads every page of a large archive. Archives with data in
    // front (self-extracting ones) fail that and are detected after all.
    let known = zip::read::Config { archive_offset: zip::read::ArchiveOffset::Known(0) };
    let mut archive = match zip::ZipArchive::with_config(known, Cursor::new(data.clone())) {
        Ok(archive) => archive,
        Err(_) => zip::ZipArchive::new(Cursor::new(data)).map_err(Error::archive)?,
    };
    let mut index_of = HashMap::new();
    for i in 0..archive.len() {
        let file = archive.by_index_raw(i).map_err(Error::archive)?;
        let Some(path) = file.name().ok().and_then(|n| normalize(&n)) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let mtime = file.last_modified().map(|dt| {
            civil_to_unix(dt.year() as i64, dt.month() as i64, dt.day() as i64)
                + dt.hour() as i64 * 3600
                + dt.minute() as i64 * 60
                + dt.second() as i64
        });
        if file.is_dir() {
            index.ensure_dir(&path, mtime);
        } else {
            let kind = if file.is_symlink() { EntryKind::Symlink } else { EntryKind::File };
            index.add_file(&path, kind, file.size(), mtime, Some(QuickId::Crc32(file.crc32())));
            index_of.insert(path, i);
        }
    }
    Ok(Backend::Zip { archive, index_of })
}

fn open_7z(data: Data, index: &mut Index) -> Result<Backend> {
    let archive = sevenz_rust2::Archive::read(&mut Cursor::new(&*data), &sevenz_rust2::Password::empty())
        .map_err(Error::archive)?;
    let mut index_of = HashMap::new();
    for (i, file) in archive.files.iter().enumerate() {
        let Some(path) = normalize(&file.name) else {
            continue;
        };
        if path.is_empty() || file.is_anti_item {
            continue;
        }
        let mtime = file.has_last_modified_date.then(|| {
            let time: std::time::SystemTime = file.last_modified_date.into();
            unix_secs(time)
        });
        if file.is_directory {
            index.ensure_dir(&path, mtime);
        } else {
            let quick_id = file.has_crc.then_some(QuickId::Crc32(file.crc as u32));
            index.add_file(&path, sevenz_kind(file), file.size, mtime, quick_id);
            index_of.insert(path, i);
        }
    }
    let mut files_per_block = vec![0usize; archive.blocks.len()];
    for block in archive.stream_map.file_block_index.iter().flatten() {
        files_per_block[*block] += 1;
    }
    let solid = files_per_block.iter().map(|&n| (n > 1).then(|| Mutex::new(None))).collect();
    Ok(Backend::SevenZ { data, archive: Box::new(archive), index_of, solid })
}

/// A range of shared data, read as a stream.
struct DataSlice {
    data: Data,
    at: usize,
    end: usize,
}

impl Read for DataSlice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.end - self.at);
        buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// A symlink, when the entry has a Unix mode, which 7-Zip stores in the high 16
/// bits of the attributes, flagged by 0x8000; otherwise a file.
fn sevenz_kind(file: &sevenz_rust2::ArchiveEntry) -> EntryKind {
    let attributes = file.windows_attributes;
    let is_link = file.has_windows_attributes && attributes & 0x8000 != 0 && (attributes >> 16) & 0o170000 == 0o120000;
    if is_link { EntryKind::Symlink } else { EntryKind::File }
}

/// The decompressed tar stream of a tar archive.
fn tar_reader(raw: &[u8], format: ArchiveFormat) -> Result<Box<dyn Read + '_>> {
    Ok(match format {
        ArchiveFormat::Tar => Box::new(raw),
        ArchiveFormat::TarGz => Box::new(flate2::read::MultiGzDecoder::new(raw)),
        ArchiveFormat::TarXz => Box::new(lzma_rust2::XzReader::new(raw, true)),
        ArchiveFormat::TarZst => Box::new(ruzstd::decoding::StreamingDecoder::new(raw).map_err(Error::archive)?),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(raw)),
        ArchiveFormat::Zip | ArchiveFormat::SevenZ => unreachable!("not a tar format"),
    })
}

fn open_tar(data: Data, format: ArchiveFormat, index: &mut Index) -> Result<Backend> {
    let compressed = !matches!(format, ArchiveFormat::Tar);
    // A compressed stream is written to the temporary file, so how far it may
    // expand has to be bounded here; an uncompressed tar is read in place.
    let limit = (data.len() as u64).saturating_mul(TAR_RATIO).clamp(TAR_MIN, TAR_MAX);
    let mut expanded = 0u64;
    let mut spool: Option<Spool> = None;
    let mut contents: HashMap<String, TarContent> = HashMap::new();
    let mut archive = tar::Archive::new(tar_reader(&data, format)?);
    for entry in archive.entries().map_err(Error::archive)? {
        let mut entry = entry.map_err(Error::archive)?;
        let Some(path) = entry.path().ok().and_then(|p| normalize(&p.to_string_lossy())) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let mtime = entry.header().mtime().ok().map(|t| t as i64);
        let kind = entry.header().entry_type();
        let content = match kind {
            tar::EntryType::Directory => {
                index.ensure_dir(&path, mtime);
                continue;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => {
                let size = entry.size();
                entry_hint(size)?;
                if !compressed && !kind.is_gnu_sparse() {
                    // The entry is already in the archive: read it from there.
                    TarContent::Direct(Span { at: entry.raw_file_position(), len: size })
                } else {
                    count(&mut expanded, size, limit)?;
                    let spool = match &mut spool {
                        Some(spool) => spool,
                        None => spool.insert(Spool::new()?),
                    };
                    TarContent::Spooled(spool.append(&mut entry, size)?)
                }
            }
            tar::EntryType::Link => {
                // A hard link names an earlier entry from the archive root: the same file.
                let target = entry.link_name().ok().flatten().and_then(|t| normalize(&t.to_string_lossy()));
                match target.as_ref().and_then(|t| contents.get(t)) {
                    Some(TarContent::Bytes(bytes)) => TarContent::Bytes(bytes.clone()),
                    Some(TarContent::Direct(span)) => TarContent::Direct(*span),
                    Some(TarContent::Spooled(span)) => TarContent::Spooled(*span),
                    None => {
                        // No such file before it: kept as a link that points at that path.
                        let depth = path.matches('/').count();
                        let buf = format!("{}{}", "../".repeat(depth), target.unwrap_or_default()).into_bytes();
                        count(&mut expanded, buf.len() as u64, limit)?;
                        TarContent::Bytes(Arc::new(buf))
                    }
                }
            }
            tar::EntryType::Symlink => {
                // Links are compared by their target path.
                let target = entry.link_name().ok().flatten().map(|t| t.to_string_lossy().into_owned());
                let buf = target.unwrap_or_default().into_bytes();
                count(&mut expanded, buf.len() as u64, limit)?;
                TarContent::Bytes(Arc::new(buf))
            }
            _ => continue,
        };
        let size = content.len();
        let entry_kind = match kind {
            tar::EntryType::Symlink => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        index.add_file(&path, entry_kind, size, mtime, None);
        contents.insert(path, content);
    }
    drop(archive);
    Ok(Backend::Tar { data, format, contents, spool })
}

/// Counts what a compressed tar expands to, refusing to go past `limit`.
fn count(expanded: &mut u64, size: u64, limit: u64) -> Result<()> {
    *expanded = expanded.saturating_add(size);
    if *expanded > limit {
        return Err(Error::Unsupported(format!(
            "the archive expands to more than {} MiB, which is more than a comparison reads",
            limit >> 20
        )));
    }
    Ok(())
}

/// Decodes all files of one 7z block.
fn decode_7z_block(
    data: &Data,
    archive: &sevenz_rust2::Archive,
    block: usize,
) -> Result<HashMap<String, Arc<Vec<u8>>>> {
    let unpacked = archive.blocks[block].get_unpack_size();
    if unpacked > MAX_BLOCK {
        return Err(Error::Unsupported(format!(
            "a block of this archive expands to {} MiB, more than a comparison reads",
            unpacked >> 20
        )));
    }
    let threads = if unpacked >= MT_BLOCK_SIZE {
        std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
    } else {
        // A decoder thread pool per small block costs more than the decoding.
        1
    };
    let password = sevenz_rust2::Password::empty();
    let mut source = Cursor::new(&**data);
    let mut files = HashMap::new();
    sevenz_rust2::BlockDecoder::new(threads, block, archive, &password, &mut source)
        .for_each_entries(&mut |entry, r| {
            if !entry.is_directory {
                let buf = read_entry(r, entry.size).map_err(std::io::Error::other)?;
                if let Some(path) = normalize(&entry.name) {
                    files.insert(path, Arc::new(buf));
                }
            }
            Ok(true)
        })
        .map_err(Error::archive)?;
    Ok(files)
}

impl Vfs for ArchiveFs {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        check_rel(dir)?;
        self.index.dirs.get(dir).cloned().ok_or_else(|| Error::NotFound(join(&self.label, dir)))
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        check_rel(path)?;
        if path.is_empty() {
            return Ok(root_entry());
        }
        find_entry(&self.index.dirs, path)
    }

    fn open(&self, path: &str) -> Result<Box<dyn Read + Send>> {
        // Zip entries are decompressed while they are read, so a comparison that
        // stops at the first difference decompresses only up to it.
        if let Backend::Zip { archive, index_of } = &self.backend {
            let i = *index_of.get(path).ok_or_else(|| Error::NotFound(join(&self.label, path)))?;
            let mut archive = archive.clone();
            let (method, start, len) = {
                let file = archive.by_index_raw(i).map_err(Error::archive)?;
                let plain = !file.encrypted();
                (plain.then(|| file.compression()), file.data_start(), file.compressed_size())
            };
            let data = archive.into_inner().into_inner();
            if let Some(start) = start
                && let Some(end) = start.checked_add(len).filter(|&end| end <= data.len() as u64)
            {
                let raw = DataSlice { data, at: start as usize, end: end as usize };
                match method {
                    Some(zip::CompressionMethod::Stored) => return Ok(Box::new(raw)),
                    Some(zip::CompressionMethod::Deflated) => {
                        return Ok(Box::new(flate2::read::DeflateDecoder::new(raw)));
                    }
                    _ => {}
                }
            }
        }
        Ok(Box::new(Cursor::new(self.read(path)?)))
    }

    fn read(&self, path: &str) -> Result<Data> {
        check_rel(path)?;
        let not_found = || Error::NotFound(join(&self.label, path));
        match &self.backend {
            Backend::Zip { archive, index_of } => {
                let i = *index_of.get(path).ok_or_else(not_found)?;
                // Cloning shares the parsed central directory; it lets reads run in parallel.
                let mut archive = archive.clone();
                let mut file = archive.by_index(i).map_err(Error::archive)?;
                let size = file.size();
                Ok(Data::from_vec(read_entry(&mut file, size)?))
            }
            Backend::SevenZ { data, archive, index_of, solid } => {
                let i = *index_of.get(path).ok_or_else(not_found)?;
                let Some(block) = archive.stream_map.file_block_index[i] else {
                    return Ok(Data::from_vec(Vec::new()));
                };
                let Some(slot) = &solid[block] else {
                    let mut files = decode_7z_block(data, archive, block)?;
                    return files.remove(path).map(Data::from_shared).ok_or_else(not_found);
                };
                // Reads of other files in the same block wait here instead of decoding it again.
                let files = {
                    let mut slot = slot.lock();
                    match &*slot {
                        Some(files) => files.clone(),
                        None => {
                            let files = Arc::new(decode_7z_block(data, archive, block)?);
                            *slot = Some(files.clone());
                            files
                        }
                    }
                };
                files.get(path).cloned().map(Data::from_shared).ok_or_else(not_found)
            }
            Backend::Tar { data, contents, spool, .. } => {
                let content = contents.get(path).ok_or_else(not_found)?;
                Ok(Data::from_vec(tar_read(data, spool.as_ref(), content)?))
            }
        }
    }
}

fn unix_secs(time: std::time::SystemTime) -> i64 {
    match time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Days-from-civil (Howard Hinnant), returned as seconds since the Unix epoch.
/// Zip timestamps carry no time zone; they are treated as UTC.
fn civil_to_unix(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_to_unix(1970, 1, 1), 0);
        assert_eq!(civil_to_unix(2000, 3, 1), 951_868_800);
    }

    #[test]
    fn large_flat_archives_index_in_linear_time() {
        let mut index = Index::new();
        let n = 200_000;
        let start = std::time::Instant::now();
        for i in 0..n {
            index.add_file(&format!("f{i}"), EntryKind::File, 1, None, None);
        }
        // A repeated entry replaces the earlier one.
        index.add_file("f7", EntryKind::File, 2, None, None);
        let root = &index.dirs[""];
        assert_eq!(root.len(), n);
        assert_eq!(root[7].size, 2);
        // Quadratic insertion took minutes for this many entries.
        assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());
    }

    #[test]
    fn formats() {
        assert_eq!(detect_format("a/B.ZIP"), Some(ArchiveFormat::Zip));
        assert_eq!(detect_format("x.tar.gz"), Some(ArchiveFormat::TarGz));
        assert_eq!(detect_format("x.7z"), Some(ArchiveFormat::SevenZ));
        assert_eq!(detect_format("x.gz"), None);
    }
}
