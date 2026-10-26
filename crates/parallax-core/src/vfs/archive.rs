//! Archives as read-only trees: zip (and jar, nupkg, …), 7z and tar with optional compression.
//!
//! The directory index is built once from the archive headers, so listing never
//! decompresses anything. zip and 7z headers carry a CRC32 per entry, which the
//! compare engine uses as a [`QuickId`].
//!
//! Writing rewrites the whole archive, see [`write`].

mod input;
mod write;
use input::{ArchiveData, ArchiveReader};

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek, SeekFrom};
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
                    ignored: false,
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
        let entry = Entry { name: name.to_string(), kind, size, mtime, quick_id, ignored: false };
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

/// Blocks at least this large are decoded with several threads, when the
/// encoder split them into independent chunks (7-Zip does for LZMA2).
const MT_BLOCK_SIZE: u64 = 16 << 20;

/// No entry of an archive is expanded beyond this. The size in a header is
/// attacker-controlled, and neither memory nor a temporary file should follow it.
const MAX_ENTRY: u64 = 2 << 30;

/// No 7z block is decoded beyond this; what does not fit in memory goes to disk.
const MAX_BLOCK: u64 = 4 << 30;

/// Decoded files of one solid 7z block kept in memory; the rest of the block goes
/// to a temporary file. A solid block can hold gigabytes.
const SOLID_MEMORY: usize = if cfg!(test) { 1 << 10 } else { 64 << 20 };

/// A solid block's decoder stops after this long without a reader waiting for it,
/// so idle archives do not hold a decoder (and its dictionary) each.
const SOLID_IDLE: std::time::Duration = std::time::Duration::from_millis(if cfg!(test) { 200 } else { 30_000 });

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
    /// Inside the uncompressed archive, read by seeking to the entry.
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
fn tar_read(data: &ArchiveData, spool: Option<&Spool>, content: &TarContent) -> Result<Vec<u8>> {
    let slice = |span: &Span| -> Result<Vec<u8>> {
        if span.at.checked_add(span.len).is_none_or(|end| end > data.len()) {
            return Err(Error::Unsupported("the tar archive is truncated".into()));
        }
        let mut reader = data.reader();
        reader.seek(SeekFrom::Start(span.at)).map_err(Error::archive)?;
        read_entry(&mut reader.take(span.len), span.len)
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
        archive: zip::ZipArchive<ArchiveReader>,
        index_of: HashMap<String, usize>,
    },
    /// 7z is decoded per block. A solid block holds many files that can only be
    /// decompressed in sequence: it is decoded by a thread of its own, only as far as
    /// reads need, and never twice (see [`SolidBlock`]). Blocks of different files
    /// decode in parallel. A block with a single file (non-solid archives) is decoded
    /// on each read, like a zip entry.
    SevenZ {
        data: ArchiveData,
        archive: Arc<sevenz_rust2::Archive>,
        index_of: HashMap<String, usize>,
        /// Per block: a solid block's decoder and what it decoded, `None` for a single-file block.
        solid: SolidBlocks,
        /// Per file: where it ends in its block's decoded data.
        ends: Vec<u64>,
    },
    /// tar streams are decompressed once while indexing; the decompressed stream
    /// is kept out of memory (see [`TarContent`]).
    Tar {
        data: ArchiveData,
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
        Self::from_input(ArchiveData::Bytes(data), format, label.into())
    }

    pub fn open_file(path: &std::path::Path, format: ArchiveFormat, label: impl Into<String>) -> Result<Self> {
        let input = ArchiveData::file(path).map_err(|e| Error::io(path.display().to_string(), e))?;
        Self::from_input(input, format, label.into())
    }

    fn from_input(data: ArchiveData, format: ArchiveFormat, label: String) -> Result<Self> {
        let mut index = Index::new();
        let backend = match format {
            ArchiveFormat::Zip => open_zip(data, &mut index)?,
            ArchiveFormat::SevenZ => open_7z(data, &mut index)?,
            _ => open_tar(data, format, &mut index)?,
        };
        Ok(ArchiveFs { label, index, backend })
    }
}

fn open_zip(data: ArchiveData, index: &mut Index) -> Result<Backend> {
    // Offset 0 first: detecting it scans the whole file for central directory
    // headers, which reads every page of a large archive. Archives with data in
    // front (self-extracting ones) fail that and are detected after all.
    let known = zip::read::Config { archive_offset: zip::read::ArchiveOffset::Known(0) };
    let archive = match zip::ZipArchive::with_config(known, data.reader()) {
        Ok(archive) => archive,
        Err(_) => zip::ZipArchive::new(data.reader()).map_err(Error::archive)?,
    };
    let mut index_of = HashMap::new();
    for i in 0..archive.len() {
        // Central metadata is sufficient; do not seek to every local member header.
        let file = archive.by_index_data(i).map_err(Error::archive)?;
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

fn open_7z(data: ArchiveData, index: &mut Index) -> Result<Backend> {
    let archive =
        sevenz_rust2::Archive::read(&mut data.reader(), &sevenz_rust2::Password::empty()).map_err(Error::archive)?;
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
    // Where each file ends in its block's decoded data: how far a read decodes.
    let mut decoded = vec![0u64; archive.blocks.len()];
    let mut ends = vec![0u64; archive.files.len()];
    for (i, block) in archive.stream_map.file_block_index.iter().enumerate() {
        if let Some(block) = *block {
            files_per_block[block] += 1;
            decoded[block] += archive.files[i].size;
            ends[i] = decoded[block];
        }
    }
    let solid =
        SolidBlocks(files_per_block.iter().map(|&n| (n > 1).then(|| Arc::new(SolidBlock::default()))).collect());
    Ok(Backend::SevenZ { data, archive: Arc::new(archive), index_of, solid, ends })
}

/// A symlink, when the entry has a Unix mode, which 7-Zip stores in the high 16
/// bits of the attributes, flagged by 0x8000; otherwise a file.
fn sevenz_kind(file: &sevenz_rust2::ArchiveEntry) -> EntryKind {
    let attributes = file.windows_attributes;
    let is_link = file.has_windows_attributes && attributes & 0x8000 != 0 && (attributes >> 16) & 0o170000 == 0o120000;
    if is_link { EntryKind::Symlink } else { EntryKind::File }
}

/// The decompressed tar stream of a tar archive.
fn tar_reader(data: &ArchiveData, format: ArchiveFormat) -> Result<Box<dyn Read>> {
    let raw = data.reader();
    Ok(match format {
        ArchiveFormat::Tar => Box::new(raw),
        ArchiveFormat::TarGz => Box::new(flate2::read::MultiGzDecoder::new(raw)),
        ArchiveFormat::TarXz => Box::new(lzma_rust2::XzReader::new(raw, true)),
        ArchiveFormat::TarZst => Box::new(ruzstd::decoding::StreamingDecoder::new(raw).map_err(Error::archive)?),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(raw)),
        ArchiveFormat::Zip | ArchiveFormat::SevenZ => unreachable!("not a tar format"),
    })
}

fn open_tar(data: ArchiveData, format: ArchiveFormat, index: &mut Index) -> Result<Backend> {
    let compressed = !matches!(format, ArchiveFormat::Tar);
    // A compressed stream is written to the temporary file, so how far it may
    // expand has to be bounded here; an uncompressed tar is read in place.
    let limit = data.len().saturating_mul(TAR_RATIO).clamp(TAR_MIN, TAR_MAX);
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

/// Refuses a block that expands beyond [`MAX_BLOCK`]; the decoder threads to use.
fn block_threads(archive: &sevenz_rust2::Archive, block: usize) -> Result<u32> {
    let unpacked = archive.blocks[block].get_unpack_size();
    if unpacked > MAX_BLOCK {
        return Err(Error::Unsupported(format!(
            "a block of this archive expands to {} MiB, more than a comparison reads",
            unpacked >> 20
        )));
    }
    Ok(if unpacked >= MT_BLOCK_SIZE {
        std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
    } else {
        // A decoder thread pool per small block costs more than the decoding.
        1
    })
}

/// Decodes the file `path` of a block that holds only it (or few files).
fn decode_7z_file(
    data: &ArchiveData,
    archive: &sevenz_rust2::Archive,
    block: usize,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    let threads = block_threads(archive, block)?;
    let password = sevenz_rust2::Password::empty();
    let mut source = data.reader();
    let mut found = None;
    sevenz_rust2::BlockDecoder::new(threads, block, archive, &password, &mut source)
        .for_each_entries(&mut |entry, r| {
            if !entry.is_directory && normalize(&entry.name).as_deref() == Some(path) {
                found = Some(read_entry(r, entry.size).map_err(std::io::Error::other)?);
                return Ok(false);
            }
            Ok(true)
        })
        .map_err(Error::archive)?;
    Ok(found)
}

/// The solid blocks of a 7z archive; dropping the archive stops their decoders.
struct SolidBlocks(Vec<Option<Arc<SolidBlock>>>);

impl Drop for SolidBlocks {
    fn drop(&mut self) {
        for block in self.0.iter().flatten() {
            block.state.lock().cancelled = true;
            block.changed.notify_all();
        }
    }
}

/// A solid 7z block: its files can only be decoded in order. A thread decodes it
/// while a reader waits for a file not decoded yet, and pauses when none does, so
/// a read decodes the block only up to its file. Decoded files are kept (in memory
/// up to [`SOLID_MEMORY`], then in a temporary file), so nothing is decoded twice.
#[derive(Default)]
struct SolidBlock {
    state: Mutex<SolidState>,
    changed: parking_lot::Condvar,
    /// Files beyond the memory budget.
    spool: Mutex<Option<Spool>>,
}

#[derive(Default)]
struct SolidState {
    files: HashMap<String, Decoded>,
    memory: usize,
    /// Readers waiting for a file not decoded yet.
    waiting: usize,
    running: bool,
    /// The threads of the running (or last) decoder.
    threads: u32,
    /// A reader wants more threads than the running decoder has: it stops, and a
    /// decoder with more starts over, skipping what is kept.
    restart: bool,
    finished: bool,
    error: Option<String>,
    cancelled: bool,
}

#[derive(Clone)]
enum Decoded {
    Memory(Arc<Vec<u8>>),
    Spooled(Span),
}

impl SolidBlock {
    /// Waits until `path` is decoded, starting the decoder when it is not running.
    fn get(
        self: &Arc<Self>,
        data: &ArchiveData,
        archive: &Arc<sevenz_rust2::Archive>,
        block: usize,
        path: &str,
        threads: u32,
    ) -> Result<Option<Decoded>> {
        let mut st = self.state.lock();
        loop {
            if let Some(found) = st.files.get(path) {
                return Ok(Some(found.clone()));
            }
            if let Some(e) = &st.error {
                return Err(Error::Unsupported(e.clone()));
            }
            if st.finished {
                return Ok(None);
            }
            // Only a single thread gives way, once: restarting costs the decoded prefix.
            if st.running && st.threads == 1 && threads > 1 {
                st.restart = true;
            }
            if !st.running {
                block_threads(archive, block)?;
                st.running = true;
                // Never fewer threads than before: another restart would decode the prefix again.
                st.threads = threads.max(st.threads);
                st.restart = false;
                let (me, data, archive) = (self.clone(), data.clone(), archive.clone());
                std::thread::Builder::new()
                    .name("7z block".into())
                    .spawn({
                        let threads = st.threads;
                        move || me.decode(&data, &archive, block, threads)
                    })
                    .map_err(|e| Error::io("a thread to decode the 7z block", e))?;
            }
            st.waiting += 1;
            self.changed.notify_all();
            self.changed.wait(&mut st);
            st.waiting -= 1;
        }
    }

    /// The bytes of a decoded file.
    fn bytes(&self, decoded: Decoded) -> Result<Data> {
        match decoded {
            Decoded::Memory(bytes) => Ok(Data::from_shared(bytes)),
            Decoded::Spooled(span) => {
                let spool = self.spool.lock();
                let spool = spool.as_ref().ok_or_else(|| Error::Unsupported("the decoded 7z block is gone".into()))?;
                Ok(Data::from_vec(spool.read(span)?))
            }
        }
    }

    /// The decoder thread: decodes the block in order, keeping the files not kept
    /// yet, and pauses whenever no reader waits. Stops when the archive is dropped,
    /// or after [`SOLID_IDLE`] without demand (a later read starts it again).
    fn decode(&self, data: &ArchiveData, archive: &sevenz_rust2::Archive, block: usize, threads: u32) {
        let password = sevenz_rust2::Password::empty();
        let mut source = data.reader();
        let result = sevenz_rust2::BlockDecoder::new(threads, block, archive, &password, &mut source).for_each_entries(
            &mut |entry, r| {
                if entry.is_directory {
                    return Ok(true);
                }
                let Some(path) = normalize(&entry.name) else { return Ok(true) };
                let known = self.state.lock().files.contains_key(&path);
                if known {
                    // Decoded before the decoder last stopped: skipped, not kept twice.
                    std::io::copy(&mut r.take(entry.size), &mut std::io::sink())?;
                } else {
                    let decoded = self.keep(r, entry.size).map_err(std::io::Error::other)?;
                    self.state.lock().files.insert(path, decoded);
                }
                let mut st = self.state.lock();
                self.changed.notify_all();
                if st.restart {
                    return Ok(false);
                }
                while st.waiting == 0 && !st.cancelled && !st.restart {
                    if self.changed.wait_for(&mut st, SOLID_IDLE).timed_out() && st.waiting == 0 {
                        return Ok(false);
                    }
                }
                Ok(!st.cancelled && !st.restart)
            },
        );
        let mut st = self.state.lock();
        st.running = false;
        match result {
            Err(e) => st.error = Some(Error::archive(e).to_string()),
            // The whole block: what a reader still waits for is not in it.
            Ok(true) => st.finished = true,
            // Stopped while idle, for a faster decoder, or dropped: a read starts it again.
            Ok(false) => {}
        }
        self.changed.notify_all();
    }

    /// Keeps a decoded file: in memory while the block's budget lasts, else on disk.
    fn keep(&self, r: &mut dyn Read, size: u64) -> Result<Decoded> {
        let hint = entry_hint(size)?;
        let fits = self.state.lock().memory + size as usize <= SOLID_MEMORY;
        if fits {
            let mut buf = Vec::with_capacity(hint);
            r.take(size).read_to_end(&mut buf).map_err(Error::archive)?;
            self.state.lock().memory += buf.len();
            return Ok(Decoded::Memory(Arc::new(buf)));
        }
        let mut spool = self.spool.lock();
        if spool.is_none() {
            *spool = Some(Spool::new()?);
        }
        let span = spool.as_mut().expect("just created").append(r, size)?;
        if span.len != size {
            return Err(Error::Unsupported("the 7z block is truncated".into()));
        }
        Ok(Decoded::Spooled(span))
    }
}

impl ArchiveFs {
    /// Reads the file `i` (`path`) of a solid block.
    fn solid_file(&self, block: &Arc<SolidBlock>, i: usize, path: &str) -> Result<Option<Decoded>> {
        let Backend::SevenZ { data, archive, ends, .. } = &self.backend else { return Ok(None) };
        let Some(b) = archive.stream_map.file_block_index[i] else { return Ok(None) };
        // Several threads decode a whole block about 2.5 times as fast as one, but in
        // chunks of tens of MB (7-Zip's LZMA2 chunks), ahead of what is read. One
        // thread stops right after the file: for the first file read, early in the
        // block, that saves most of the work. Any further file not decoded yet
        // switches to several threads (the single one stops; see `restart`), so a
        // block with changes all over decodes at full speed, having lost at most the
        // first 15 % to one thread. Two blocks decoding side by side each get all
        // threads: halving them would take two rounds of chunks instead of one.
        let first = block.state.lock().files.is_empty();
        let percent = ends[i].saturating_mul(100) / archive.blocks[b].get_unpack_size().max(1);
        let threads = match block_threads(archive, b)? {
            _ if first && percent < 15 => 1,
            all => all,
        };
        block.get(data, archive, b, path, threads)
    }

    /// The solid 7z block holding `path`, if it is in one.
    fn solid_block_of(&self, path: &str) -> Option<&Arc<SolidBlock>> {
        let Backend::SevenZ { archive, index_of, solid, .. } = &self.backend else { return None };
        let block = archive.stream_map.file_block_index[*index_of.get(path)?]?;
        solid.0[block].as_ref()
    }
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
            let mut reader = archive.into_inner();
            if let Some(start) = start {
                reader.seek(SeekFrom::Start(start)).map_err(Error::archive)?;
                let raw = reader.take(len);
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

    fn slow_to_read(&self, path: &str) -> bool {
        self.solid_block_of(path).is_some_and(|block| !block.state.lock().files.contains_key(path))
    }

    fn warm(&self, path: &str) {
        if let (Some(block), Backend::SevenZ { index_of, .. }) = (self.solid_block_of(path), &self.backend)
            && let Some(&i) = index_of.get(path)
        {
            // A failure shows when the file is read.
            let _ = self.solid_file(block, i, path);
        }
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
            Backend::SevenZ { data, archive, index_of, solid, .. } => {
                let i = *index_of.get(path).ok_or_else(not_found)?;
                let Some(block) = archive.stream_map.file_block_index[i] else {
                    return Ok(Data::from_vec(Vec::new()));
                };
                match &solid.0[block] {
                    Some(solid) => {
                        let decoded = self.solid_file(solid, i, path)?.ok_or_else(not_found)?;
                        solid.bytes(decoded)
                    }
                    None => decode_7z_file(data, archive, block, path)?.map(Data::from_vec).ok_or_else(not_found),
                }
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

    /// A solid block decodes as far as reads need, keeps what it decoded (beyond the
    /// memory budget on disk), and serves reads in any order and from any thread.
    #[test]
    fn solid_7z_blocks_decode_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let files: Vec<(String, String)> =
            (0..40).map(|i| (format!("d{}/f{i:02}.txt", i % 4), format!("{i}:").repeat(100 + i))).collect();
        for (name, text) in &files {
            let path = src.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let archive = dir.path().join("pack.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
        writer.push_source_path(&src, |_| true).unwrap();
        writer.finish().unwrap();

        let fs = ArchiveFs::open_file(&archive, ArchiveFormat::SevenZ, "pack.7z").unwrap();
        let Backend::SevenZ { solid, .. } = &fs.backend else { panic!("a 7z archive") };
        assert!(solid.0.iter().flatten().count() >= 1, "the archive is solid");
        assert!(files.iter().all(|(name, _)| fs.slow_to_read(name)));
        // Backwards, then from several threads at once, then again.
        for (name, text) in files.iter().rev() {
            assert_eq!(&*fs.read(name).unwrap(), text.as_bytes(), "{name}");
        }
        assert!(files.iter().all(|(name, _)| !fs.slow_to_read(name)));
        std::thread::scope(|s| {
            for chunk in files.chunks(10) {
                let fs = &fs;
                s.spawn(move || {
                    for (name, text) in chunk {
                        assert_eq!(&*fs.read(name).unwrap(), text.as_bytes(), "{name}");
                    }
                });
            }
        });
        let block = solid.0.iter().flatten().next().unwrap();
        assert!(block.spool.lock().is_some(), "files beyond the memory budget are kept on disk");
        assert!(block.state.lock().memory <= SOLID_MEMORY);
        assert!(fs.read("d0/missing.txt").is_err());

        // Warming decodes as far as the file, and the read is then quick.
        let fresh = ArchiveFs::open_file(&archive, ArchiveFormat::SevenZ, "pack.7z").unwrap();
        let (name, text) = &files[3];
        fresh.warm(name);
        assert!(!fresh.slow_to_read(name));
        assert_eq!(&*fresh.read(name).unwrap(), text.as_bytes());

        // Left idle, the decoder stops; a later read starts it again and gets the rest.
        let Backend::SevenZ { solid, .. } = &fresh.backend else { unreachable!() };
        let running = || solid.0.iter().flatten().any(|b| b.state.lock().running);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!running(), "an idle decoder stops");
        for (name, text) in &files {
            assert_eq!(&*fresh.read(name).unwrap(), text.as_bytes(), "{name}");
        }
    }

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
