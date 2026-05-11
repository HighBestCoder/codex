use std::io;

#[derive(Debug, thiserror::Error)]
pub enum CallGraphStoreError {
    #[error("sled error: {0}")]
    Sled(#[from] sled::Error),
    #[error("postcard serialization error: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("integrity error: {0}")]
    Integrity(String),
    #[error("string id {0} not found in dictionary")]
    UnknownStringId(u32),
    #[error("u32 id space exhausted while allocating {what}")]
    IdExhausted { what: &'static str },
}
