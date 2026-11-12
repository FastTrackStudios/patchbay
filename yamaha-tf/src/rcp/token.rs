//! Tokenizer and string quoting.
//!
//! Tokens are separated by one or more spaces. String arguments are
//! double-quoted; inside quotes a backslash escapes the next character
//! (`\"` and `\\` in practice).

use crate::error::{Result, TfError};

/// One token of an RCP line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Unescaped text (without the quotes).
    pub text: String,
    /// Whether the token was a quoted string.
    pub quoted: bool,
}

impl Token {
    /// A bare (unquoted) token.
    #[must_use]
    pub fn bare(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            quoted: false,
        }
    }

    /// A quoted-string token.
    #[must_use]
    pub fn quoted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            quoted: true,
        }
    }
}

/// Split one line (without its LF) into tokens.
///
/// Lenient: an unterminated quoted string runs to the end of the line,
/// and a trailing lone backslash is dropped.
#[must_use]
pub fn tokenize(line: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(char::is_ascii_whitespace).is_some() {}
        let Some(&first) = chars.peek() else {
            break;
        };
        let mut text = String::new();
        if first == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            text.push(n);
                        }
                    }
                    '"' => break,
                    _ => text.push(c),
                }
            }
            out.push(Token::quoted(text));
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_ascii_whitespace()) {
                text.push(c);
            }
            out.push(Token::bare(text));
        }
    }
    out
}

/// Quote `s` for the wire: wrap in `"` and escape `"` and `\`.
///
/// # Errors
/// [`TfError::Invalid`] when `s` contains a line break or NUL, which
/// would end or corrupt the command line.
pub fn quote(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' | '\0' => {
                return Err(TfError::Invalid(format!(
                    "string {s:?} contains a line break or NUL"
                )));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// Whether `s` can be sent as a bare word (address, verb, keyword).
#[must_use]
pub fn is_bare_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
}
