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
    #[error("git: {0}")]
    Git(String),
    #[error("archive: {0}")]
    Archive(String),
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
        Error::Io { path: path.into(), source }
    }

    pub fn git(err: impl std::fmt::Display) -> Self {
        Error::Git(err.to_string())
    }

    pub fn archive(err: impl std::fmt::Display) -> Self {
        Error::Archive(err.to_string())
    }
}
