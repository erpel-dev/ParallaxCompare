//! Seekable archive input. Local containers retain a file handle, not a Windows
//! memory mapping or an eager full-file copy. Each reader has its own position.
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::vfs::Data;

#[derive(Clone)]
pub(super) enum ArchiveData {
    Bytes(Data),
    File(Arc<File>, u64),
}

impl ArchiveData {
    pub fn file(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self::File(Arc::new(file), len))
    }
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(data) => data.len() as u64,
            Self::File(_, len) => *len,
        }
    }
    pub fn reader(&self) -> ArchiveReader {
        match self {
            Self::Bytes(data) => ArchiveReader::Bytes(Cursor::new(data.clone())),
            Self::File(file, len) => ArchiveReader::File(BufReader::with_capacity(
                64 * 1024,
                FileCursor { file: file.clone(), len: *len, pos: 0 },
            )),
        }
    }
}

pub(super) struct FileCursor {
    file: Arc<File>,
    len: u64,
    pos: u64,
}

impl Read for FileCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt;
            self.file.read_at(buf, self.pos)?
        };
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt;
            self.file.seek_read(buf, self.pos)?
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for FileCursor {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let pos = match from {
            SeekFrom::Start(pos) => {
                self.pos = pos;
                return Ok(pos);
            }
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid archive seek"))?;
        self.pos = pos;
        Ok(pos)
    }
}

pub(super) enum ArchiveReader {
    Bytes(Cursor<Data>),
    File(BufReader<FileCursor>),
}

impl Clone for ArchiveReader {
    fn clone(&self) -> Self {
        match self {
            Self::Bytes(cursor) => Self::Bytes(cursor.clone()),
            Self::File(reader) => {
                let file = reader.get_ref();
                Self::File(BufReader::with_capacity(
                    64 * 1024,
                    FileCursor { file: file.file.clone(), len: file.len, pos: file.pos - reader.buffer().len() as u64 },
                ))
            }
        }
    }
}

impl Read for ArchiveReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Bytes(r) => r.read(buf),
            Self::File(r) => r.read(buf),
        }
    }
}
impl Seek for ArchiveReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Bytes(r) => r.seek(from),
            Self::File(r) => r.seek(from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_file_readers_keep_independent_logical_positions() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        file.write_all(b"0123456789abcdef").unwrap();
        let input = ArchiveData::file(file.path()).unwrap();
        let mut a = input.reader();
        let mut first = [0; 3];
        a.read_exact(&mut first).unwrap(); // Buffered cursor is ahead of logical position.
        let mut b = a.clone();
        let mut next = [0; 3];
        b.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"345");
        a.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"345");
        b.seek(SeekFrom::End(-3)).unwrap();
        b.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"def");
        a.seek(SeekFrom::Current(-2)).unwrap();
        a.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"456");
        assert!(b.seek(SeekFrom::Start(0)).is_ok());
        assert!(b.seek(SeekFrom::Current(-1)).is_err());
    }
}
