//! The lossless layer: splits the raw source into an ordered sequence of
//! records (section headers, property lines, blank lines, and "unknown"
//! lines that could not be classified) without interpreting any value
//! content.
//!
//! Every record keeps a "full span" that is authoritative for byte-exact
//! output: [`crate::document::Document::serialize`] simply walks the
//! records in order and copies `source[full_span]` for each one. Because
//! [`scan`] guarantees the full spans partition the source contiguously
//! (no gaps, no overlaps, no reordering), replaying them always reproduces
//! the input exactly - this is the whole round-trip guarantee, and it
//! holds regardless of whether the finer-grained sub-spans (key, value,
//! header attributes) were classified correctly.

use crate::error::Diagnostic;
use crate::scan;
use crate::span::{column_of, line_of, Span};

/// A single `key = value` property line inside a section body.
#[derive(Debug, Clone)]
pub(crate) struct PropertyChunk {
    /// Whole line, including leading indentation (if any) and the trailing
    /// newline (if present). Authoritative for output.
    pub full_span: Span,
    pub key_span: Span,
    /// Raw text from just after `=` to just before the line terminator.
    /// Callers must `.trim()` this before structural parsing; it may carry
    /// incidental leading/trailing whitespace and, for CRLF files, a
    /// trailing `\r`.
    pub value_span: Span,
}

/// One line inside a section body that is not a recognized property.
#[derive(Debug, Clone)]
pub(crate) enum BodyLine {
    Property(PropertyChunk),
    /// Whitespace-only line (still includes its terminator).
    Blank(Span),
    /// A line that could not be classified as a property. Preserved
    /// verbatim so the document still round-trips and so tolerant parsing
    /// can proceed past minor corruption.
    Unknown(Span),
}

impl BodyLine {
    pub fn full_span(&self) -> Span {
        match self {
            BodyLine::Property(p) => p.full_span.clone(),
            BodyLine::Blank(s) | BodyLine::Unknown(s) => s.clone(),
        }
    }
}

/// A `[kind attr=value ...]` section header plus the body lines that
/// follow it, up to (but excluding) the next header or EOF.
#[derive(Debug, Clone)]
pub(crate) struct SectionChunk {
    /// Whole header line, including the trailing newline. Authoritative
    /// for output.
    pub header_span: Span,
    /// The text strictly between `[` and `]`, for structural parsing via
    /// [`crate::value::parse_header_inner`].
    pub inner_span: Span,
    pub body: Vec<BodyLine>,
}

/// The result of the lossless scan: every byte of the source is accounted
/// for by exactly one span reachable from `leading` or `sections`.
#[derive(Debug, Clone, Default)]
pub(crate) struct RawDocument {
    /// Lines appearing before the first section header. Normally empty;
    /// tolerated for robustness.
    pub leading: Vec<BodyLine>,
    pub sections: Vec<SectionChunk>,
}

fn push_body_line(sections: &mut [SectionChunk], leading: &mut Vec<BodyLine>, line: BodyLine) {
    match sections.last_mut() {
        Some(last) => last.body.push(line),
        None => leading.push(line),
    }
}

/// Scan `source` into a [`RawDocument`]. Never fails and never panics:
/// anything that cannot be confidently classified becomes a
/// [`BodyLine::Unknown`] and a [`Diagnostic`] is recorded describing why.
pub(crate) fn scan(source: &str) -> (RawDocument, Vec<Diagnostic>) {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut pos = 0usize;
    let mut diagnostics = Vec::new();
    let mut leading: Vec<BodyLine> = Vec::new();
    let mut sections: Vec<SectionChunk> = Vec::new();

    // A leading UTF-8 BOM is not whitespace and would otherwise make the
    // very first section header unrecognizable (it no longer starts with
    // '['). Carve it off as its own preserved-verbatim chunk so the header
    // right after it still parses structurally.
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    if bytes.starts_with(BOM) {
        leading.push(BodyLine::Unknown(0..BOM.len()));
        pos = BOM.len();
    }

    while pos < len {
        let record_start = pos;
        let first = scan::skip_inline_ws(bytes, pos);

        // Blank (whitespace-only) line.
        if first >= len || bytes[first] == b'\n' || bytes[first] == b'\r' {
            let line_end = scan::find_line_end(bytes, first);
            push_body_line(&mut sections, &mut leading, BodyLine::Blank(record_start..line_end));
            pos = line_end;
            continue;
        }

        // Section header.
        if bytes[first] == b'[' {
            match scan::scan_header(bytes, first) {
                Ok(header_close) => {
                    let line_end = scan::find_line_end(bytes, header_close);
                    let inner_span = (first + 1)..(header_close - 1);
                    sections.push(SectionChunk {
                        header_span: record_start..line_end,
                        inner_span,
                        body: Vec::new(),
                    });
                    pos = line_end;
                }
                Err(eof) => {
                    diagnostics.push(Diagnostic {
                        line: line_of(source, first),
                        column: column_of(source, first),
                        message: "unterminated section header: missing closing ']'".to_string(),
                    });
                    push_body_line(&mut sections, &mut leading, BodyLine::Unknown(record_start..eof));
                    pos = eof;
                }
            }
            continue;
        }

        // Property line: `key = value`.
        if let Some(key_end) = scan::scan_key(bytes, first) {
            let after_key = scan::skip_inline_ws(bytes, key_end);
            if after_key < len && bytes[after_key] == b'=' {
                let value_start = after_key + 1;
                let value_scan = scan::scan_value_span(bytes, value_start);
                if !value_scan.well_formed {
                    diagnostics.push(Diagnostic {
                        line: line_of(source, value_start),
                        column: column_of(source, value_start),
                        message: "unterminated value: unbalanced brackets or unclosed string".to_string(),
                    });
                }
                let line_end = scan::find_line_end(bytes, value_scan.end);
                let prop = PropertyChunk {
                    full_span: record_start..line_end,
                    key_span: first..key_end,
                    value_span: value_start..value_scan.end,
                };
                push_body_line(&mut sections, &mut leading, BodyLine::Property(prop));
                pos = line_end;
                continue;
            }
        }

        // Fallback: unrecognized line, preserved verbatim.
        let line_end = scan::find_line_end(bytes, first);
        push_body_line(&mut sections, &mut leading, BodyLine::Unknown(record_start..line_end));
        pos = line_end;
    }

    (RawDocument { leading, sections }, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_spans_contiguous(source: &str, doc: &RawDocument) {
        let mut spans: Vec<Span> = Vec::new();
        for line in &doc.leading {
            spans.push(line.full_span());
        }
        for section in &doc.sections {
            spans.push(section.header_span.clone());
            for line in &section.body {
                spans.push(line.full_span());
            }
        }
        let mut cursor = 0usize;
        for span in spans {
            assert_eq!(span.start, cursor, "gap or overlap before byte {cursor}");
            cursor = span.end;
        }
        assert_eq!(cursor, source.len(), "spans did not cover whole source");
    }

    #[test]
    fn covers_whole_source_for_simple_file() {
        let src = "[gd_scene load_steps=1 format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\nvisible = true\n";
        let (doc, diags) = scan(src);
        assert!(diags.is_empty());
        assert_eq!(doc.sections.len(), 2);
        all_spans_contiguous(src, &doc);
    }

    #[test]
    fn handles_multiline_string_value() {
        let src = "[gd_resource type=\"Shader\" format=3]\n\n[resource]\ncode = \"line one\nline two\"\n";
        let (doc, diags) = scan(src);
        assert!(diags.is_empty());
        all_spans_contiguous(src, &doc);
        // The multi-line string should be captured as a single property.
        let body = &doc.sections[1].body;
        assert_eq!(body.len(), 1);
        assert!(matches!(&body[0], BodyLine::Property(_)));
    }

    #[test]
    fn crlf_file_covers_whole_source() {
        let src = "[gd_scene load_steps=1 format=3]\r\n\r\n[node name=\"Root\" type=\"Node2D\"]\r\nvisible = true\r\n";
        let (doc, diags) = scan(src);
        assert!(diags.is_empty());
        all_spans_contiguous(src, &doc);
    }

    #[test]
    fn unterminated_header_recovers_with_diagnostic() {
        let src = "[gd_scene format=3\nrest of file";
        let (doc, diags) = scan(src);
        assert_eq!(diags.len(), 1);
        all_spans_contiguous(src, &doc);
    }

    #[test]
    fn unterminated_string_recovers_with_diagnostic() {
        let src = "[gd_resource type=\"X\" format=3]\n\n[resource]\ncode = \"never closed\n";
        let (doc, diags) = scan(src);
        assert_eq!(diags.len(), 1);
        all_spans_contiguous(src, &doc);
    }
}
