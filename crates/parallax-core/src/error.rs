use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("Drive is full at {path}. Free space on that drive and retry. ({source})")]
    StorageFull {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("git: {0}")]
    Git(String),
    #[error("archive: {0}")]
    Archive(String),
    #[error("{0}")]
    ArchiveResource(String),
    #[error("invalid source: {0}")]
    InvalidSource(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: impl Into<String>, source: io::Error) -> Self {
        let path = path.into();
        if storage_full(&source) { Error::StorageFull { path, source } } else { Error::Io { path, source } }
    }

    pub fn git(err: impl std::fmt::Display) -> Self {
        Error::Git(err.to_string())
    }

    pub fn archive(err: impl std::fmt::Display) -> Self {
        Error::Archive(err.to_string())
    }
}

/// Rust maps most disk-full errors; Windows also reports HANDLE_DISK_FULL (39).
pub(crate) fn storage_full(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::StorageFull || (cfg!(windows) && matches!(error.raw_os_error(), Some(39 | 112)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_drive_errors_name_the_destination_and_remedy() {
        let error = Error::io("C:/work/settings.arxml", io::Error::from(io::ErrorKind::StorageFull));
        let message = error.to_string();
        assert!(message.contains("Drive is full") && message.contains("settings.arxml") && message.contains("retry"));
        assert!(std::error::Error::source(&error).is_some());
        #[cfg(windows)]
        for code in [39, 112] {
            assert!(matches!(Error::io("destination", io::Error::from_raw_os_error(code)), Error::StorageFull { .. }));
        }
    }
}
