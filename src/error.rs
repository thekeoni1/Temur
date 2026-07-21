#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("usage: {0}")]
    Usage(String),
    #[error(transparent)]
    Args(#[from] lexopt::Error),
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls probe: {0}")]
    Tls(String),
    #[error("secret: {0}")]
    Secret(String),
    #[error("session: {0}")]
    Session(String),
}
