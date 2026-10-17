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

use super::{Data, Entry, EntryKind, QuickId, Vfs, find_entry, join, normalize, root_entry, split_parent};
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
    Some(if ends(&[".zip", ".jar", ".war", ".ear", ".apk", ".nupkg", ".vsix", ".whl", ".aar", ".xpi"]) {
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
}

impl Index {
    fn new() -> Self {
        let mut index = Index::default();
        index.dirs.insert(String::new(), Vec::new());
        index.known_dirs.insert(String::new());
        index
    }

    fn ensure_dir(&mut self, path: &str, mtime: Option<i64>) {
        if self.known_dirs.contains(path) {
            return;
        }
        let (parent, name) = split_parent(path);
        self.ensure_dir(parent, None);
        self.known_dirs.insert(path.to_string());
        self.dirs.entry(path.to_string()).or_default();
        self.dirs.get_mut(parent).expect("parent dir exists").push(Entry {
            name: name.to_string(),
            kind: EntryKind::Dir,
            size: 0,
            mtime,
            quick_id: None,
        });
    }

    fn add_file(&mut self, path: &str, kind: EntryKind, size: u64, mtime: Option<i64>, quick_id: Option<QuickId>) {
        let (parent, name) = split_parent(path);
        self.ensure_dir(parent, None);
        let list = self.dirs.get_mut(parent).expect("parent dir exists");
        // Later entries win, like extraction would behave.
        list.retain(|e| e.name != name);
        list.push(Entry { name: name.to_string(), kind, size, mtime, quick_id });
    }
}

type Extracted = Arc<HashMap<String, Arc<Vec<u8>>>>;

/// Blocks at least this large are decoded with several threads, when the
/// encoder split them into independent chunks (7-Zip does for LZMA2).
const MT_BLOCK_SIZE: u64 = 16 << 20;

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
    /// tar streams are decompressed once while indexing.
    Tar {
        data: Data,
        format: ArchiveFormat,
        contents: HashMap<String, Arc<Vec<u8>>>,
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
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).map_err(Error::archive)?;
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
            index.add_file(&path, EntryKind::File, file.size, mtime, quick_id);
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
    let mut archive = tar::Archive::new(tar_reader(&data, format)?);
    let mut contents = HashMap::new();
    for entry in archive.entries().map_err(Error::archive)? {
        let mut entry = entry.map_err(Error::archive)?;
        let Some(path) = entry.path().ok().and_then(|p| normalize(&p.to_string_lossy())) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let mtime = entry.header().mtime().ok().map(|t| t as i64);
        match entry.header().entry_type() {
            tar::EntryType::Directory => index.ensure_dir(&path, mtime),
            tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => {
                let mut buf = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buf).map_err(Error::archive)?;
                index.add_file(&path, EntryKind::File, buf.len() as u64, mtime, None);
                contents.insert(path, Arc::new(buf));
            }
            tar::EntryType::Symlink | tar::EntryType::Link => {
                // Links are compared by their target path.
                let target = entry.link_name().ok().flatten().map(|t| t.to_string_lossy().into_owned());
                let buf = target.unwrap_or_default().into_bytes();
                index.add_file(&path, EntryKind::Symlink, buf.len() as u64, mtime, None);
                contents.insert(path, Arc::new(buf));
            }
            _ => {}
        }
    }
    drop(archive);
    Ok(Backend::Tar { data, format, contents })
}

/// Decodes all files of one 7z block.
fn decode_7z_block(
    data: &Data,
    archive: &sevenz_rust2::Archive,
    block: usize,
) -> Result<HashMap<String, Arc<Vec<u8>>>> {
    let threads = if archive.blocks[block].get_unpack_size() >= MT_BLOCK_SIZE {
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
                let mut buf = Vec::with_capacity(entry.size as usize);
                r.read_to_end(&mut buf)?;
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
        self.index.dirs.get(dir).cloned().ok_or_else(|| Error::NotFound(join(&self.label, dir)))
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        if path.is_empty() {
            return Ok(root_entry());
        }
        find_entry(&self.index.dirs, path)
    }

    fn read(&self, path: &str) -> Result<Data> {
        let not_found = || Error::NotFound(join(&self.label, path));
        match &self.backend {
            Backend::Zip { archive, index_of } => {
                let i = *index_of.get(path).ok_or_else(not_found)?;
                // Cloning shares the parsed central directory; it lets reads run in parallel.
                let mut archive = archive.clone();
                let mut file = archive.by_index(i).map_err(Error::archive)?;
                let mut buf = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut buf).map_err(Error::archive)?;
                Ok(Data::from_vec(buf))
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
            Backend::Tar { contents, .. } => contents.get(path).cloned().map(Data::from_shared).ok_or_else(not_found),
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
    fn formats() {
        assert_eq!(detect_format("a/B.ZIP"), Some(ArchiveFormat::Zip));
        assert_eq!(detect_format("x.tar.gz"), Some(ArchiveFormat::TarGz));
        assert_eq!(detect_format("x.7z"), Some(ArchiveFormat::SevenZ));
        assert_eq!(detect_format("x.gz"), None);
    }
}
