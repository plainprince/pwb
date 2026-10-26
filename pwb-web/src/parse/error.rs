#[derive(Debug)]
pub enum ParseError {
    EmptySource,
    InvalidTag(String),
    SyntaxError(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for ParseError {
    fn from(err: std::io::Error) -> Self {
        ParseError::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_convert_into_parse_errors() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "locked");
        let err: ParseError = io.into();
        match err {
            ParseError::Io(inner) => {
                assert_eq!(inner.kind(), std::io::ErrorKind::PermissionDenied)
            }
            other => panic!("expected Io variant, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_variants_keep_their_diagnostics() {
        assert!(format!("{:?}", ParseError::EmptySource).contains("EmptySource"));
        let invalid = ParseError::InvalidTag("wat".to_string());
        assert!(format!("{invalid:?}").contains("wat"));
        let syntax = ParseError::SyntaxError("line 3".to_string());
        assert!(format!("{syntax:?}").contains("line 3"));
    }
}
