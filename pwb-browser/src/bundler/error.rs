//! Bundler error type.

#[derive(Debug)]
pub enum BundlerError {
    Io(std::io::Error),
    InvalidManifest(String),
}

impl From<std::io::Error> for BundlerError {
    fn from(err: std::io::Error) -> Self {
        BundlerError::Io(err)
    }
}
