use std::fmt::{self, Display, Formatter};
use std::io;
use std::result;
use super::*;

/// Error kind enumeration
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// Configuration error
    Config,
    /// I/O error
    Io,
    /// Terminal error
    Terminal,
    /// API error
    Api,
    /// Authentication error
    Auth,
    /// File system error
    FileSystem,
    /// Unknown error
    Unknown,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::Config => write!(f, "Configuration error"),
            ErrorKind::Io => write!(f, "I/O error"),
            ErrorKind::Terminal => write!(f, "Terminal error"),
            ErrorKind::Api => write!(f, "API error"),
            ErrorKind::Auth => write!(f, "Authentication error"),
            ErrorKind::FileSystem => write!(f, "File system error"),
            ErrorKind::Unknown => write!(f, "Unknown error"),
        }
    }
}

/// Format error for display in UI
pub fn format_error(error: &anyhow::Error) -> String {
    let mut result = String::new();
    
    // Extract the main error message
    result.push_str(&format!("Error: {}", error));
    
    // Add the cause chain
    let mut source = error.source();
    let mut indent = 1;
    
    while let Some(err) = source {
        result.push_str(&format!("\n{:indent$}Caused by: {}", "", err, indent = indent * 2));
        source = err.source();
        indent += 1;
    }
    
    result
}

