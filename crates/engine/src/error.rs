pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupted data: {0}")]
    Corrupt(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("fst error: {0}")]
    Fst(#[from] fst::Error),
    /// An error caused by the request. `code` follows Meilisearch error codes.
    #[error("{message}")]
    User { code: &'static str, message: String },
}

impl Error {
    pub fn user(code: &'static str, message: impl Into<String>) -> Self {
        Error::User { code, message: message.into() }
    }
    pub fn corrupt(message: impl Into<String>) -> Self {
        Error::Corrupt(message.into())
    }
}
