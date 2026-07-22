//! Error and diagnostic types. scenegraph-core never panics on malformed
//! input; parse failures are always surfaced as values.

use std::fmt;

/// A single recoverable problem found while scanning the document.
///
/// In tolerant parsing, diagnostics are collected and the offending text is
/// preserved verbatim as an "unknown" chunk so the rest of the file can
/// still be parsed and the document can still round-trip. In strict
/// parsing, the first diagnostic becomes a [`ParseError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.column, self.message)
    }
}

/// A hard parse failure, returned by [`crate::Document::parse`] (strict
/// mode) when the input could not be fully understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse error at line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl From<Diagnostic> for ParseError {
    fn from(d: Diagnostic) -> Self {
        ParseError {
            line: d.line,
            column: d.column,
            message: d.message,
        }
    }
}

/// Failure to reconstruct the node tree from `parent="..."` attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    /// The document has no `[node ...]` sections at all.
    NoNodes,
    /// More than one node was found with no `parent` attribute.
    MultipleRoots { first: String, second: String },
    /// A node's `parent` path does not resolve to any previously seen node.
    OrphanNode { name: String, parent: String },
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeError::NoNodes => write!(f, "document contains no node sections"),
            TreeError::MultipleRoots { first, second } => {
                write!(f, "multiple root nodes found: '{first}' and '{second}'")
            }
            TreeError::OrphanNode { name, parent } => {
                write!(f, "node '{name}' references unknown parent path '{parent}'")
            }
        }
    }
}

impl std::error::Error for TreeError {}

/// Failure to parse a single variant literal (a property or attribute
/// value). Carries a byte offset relative to the start of the value text
/// that was being parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "value parse error at offset {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ValueError {}
