//! Parse and encode error types.

#[derive(Debug)]
pub enum ParseError<'a> {
    Json(serde_json_core::de::Error),
    UnknownMethod(&'a str),
    MissingField(&'static str),
    InvalidShape,
    LineTooLong,
}

impl From<serde_json_core::de::Error> for ParseError<'_> {
    fn from(error: serde_json_core::de::Error) -> Self {
        ParseError::Json(error)
    }
}

#[derive(Debug)]
pub enum EncodeError {
    BufferFull,
}
