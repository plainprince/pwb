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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_convert_into_bundler_errors() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such site");
        let err: BundlerError = io.into();
        match err {
            BundlerError::Io(inner) => assert_eq!(inner.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected Io variant, got {other:?}"),
        }
    }

    #[test]
    fn invalid_manifest_carries_its_message() {
        let err = BundlerError::InvalidManifest("missing index.pml".to_string());
        assert!(format!("{err:?}").contains("missing index.pml"));
    }
}
