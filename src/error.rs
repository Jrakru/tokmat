use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Invalid pattern: {0}")]
    InvalidPattern(String),

    #[error("Invalid model: {0}")]
    InvalidModel(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid token definition: {0}")]
    InvalidTokenDefinition(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = ParseError::InvalidPattern("unbalanced <<>>".to_string());
        assert_eq!(format!("{err}"), "Invalid pattern: unbalanced <<>>");

        let err = ParseError::InvalidModel("missing tokenizer directory".to_string());
        assert_eq!(
            format!("{err}"),
            "Invalid model: missing tokenizer directory"
        );

        let err = ParseError::InvalidTokenDefinition("bad regex in .param2".to_string());
        assert_eq!(
            format!("{err}"),
            "Invalid token definition: bad regex in .param2"
        );
    }
}
