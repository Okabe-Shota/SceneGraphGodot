//! Byte-offset spans into the original source text.
//!
//! Every piece of structure that scenegraph-core recognizes keeps a [`Span`]
//! pointing back into the original source string. Serialization never
//! re-renders text from a parsed model; it always copies the bytes covered
//! by these spans. That is what makes round-tripping byte-exact: as long as
//! spans partition the source without gaps or overlaps, replaying them
//! reproduces the input exactly, whitespace and all.

/// A half-open byte range `start..end` into the document's source string.
pub type Span = std::ops::Range<usize>;

/// Compute the 1-based line number of a byte offset within `source`.
///
/// Used only for error/diagnostic reporting; not on any hot path.
pub fn line_of(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    1 + source.as_bytes()[..offset].iter().filter(|&&b| b == b'\n').count()
}

/// Compute the 1-based column number (in bytes since the last newline) of a
/// byte offset within `source`.
pub fn column_of(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    let bytes = &source.as_bytes()[..offset];
    match bytes.iter().rposition(|&b| b == b'\n') {
        Some(nl) => offset - nl,
        None => offset + 1,
    }
}
