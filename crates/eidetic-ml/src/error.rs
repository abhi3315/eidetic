use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("model load failed: {0}")]
    ModelLoad(String),

    #[error("model download failed: {0}")]
    ModelDownload(String),

    #[error("inference failed: {0}")]
    Inference(String),

    #[error("tokenizer error: {0}")]
    Tokenize(String),
}

pub type Result<T> = std::result::Result<T, Error>;
