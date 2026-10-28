//! The access token the hub issues at pairing.

use std::fmt;

/// Access token for the hub's local API. It controls every device in the home, so it never
/// shows up in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token itself, for storing it and for the `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub(crate) fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_stays_out_of_debug_output() {
        let debug = format!("{:?}", Some(Token::new("SECRET-TOKEN")));
        assert!(!debug.contains("SECRET-TOKEN"), "{debug}");
        assert!(debug.contains("redacted"));
    }
}
