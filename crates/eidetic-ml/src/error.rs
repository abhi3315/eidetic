use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("model error: {0}")]
    Model(String),
}

pub type Result<T> = std::result::Result<T, Error>;
