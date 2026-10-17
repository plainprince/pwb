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
