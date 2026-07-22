//! Low-level byte scanning helpers shared by the raw chunk splitter
//! ([`crate::raw`]) and the variant value lexer ([`crate::value`]).
//!
//! Everything here operates on raw bytes rather than `char`s. This is safe
//! for UTF-8 input because every delimiter byte we look for (`"`, `\`,
//! brackets, newline, ASCII whitespace) is below 0x80, and no byte of a
//! multi-byte UTF-8 sequence is ever below 0x80. So scanning for these
//! bytes can never land on / split a multi-byte character, and every index
//! this module returns is a valid `str` char boundary.

/// Find the end of the current line starting the scan at `pos`: the index
/// just after the next `\n`, or `bytes.len()` if there is no more `\n`
/// (i.e. the file's last line has no trailing newline).
pub fn find_line_end(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// Skip ASCII spaces and tabs (not newlines) starting at `pos`.
pub fn skip_inline_ws(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

/// Scan a double-quoted string literal starting at `bytes[start] == b'"'`.
///
/// Handles backslash escapes (the char after `\` is always skipped without
/// interpretation) and tolerates literal, unescaped newlines inside the
/// string body (Godot text resources may embed raw multi-line text, e.g.
/// shader source, inside a single quoted string).
///
/// Returns `Ok(end)` where `end` is the index just past the closing quote,
/// or `Err(bytes.len())` if the string is never closed before EOF.
pub fn scan_string(bytes: &[u8], start: usize) -> Result<usize, usize> {
    debug_assert_eq!(bytes.get(start), Some(&b'"'));
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                // Skip the escaped character. If backslash is the very
                // last byte of the file, stop; this is unterminated.
                if i + 1 >= bytes.len() {
                    return Err(bytes.len());
                }
                i += 2;
            }
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err(bytes.len())
}

/// Outcome of scanning a value expression (see [`scan_value_span`]).
pub struct ValueScan {
    /// Index just past the last byte belonging to the value.
    pub end: usize,
    /// `false` if we ran off the end of the input with unbalanced
    /// brackets/parens or an unterminated string still open. The value is
    /// still returned best-effort (extending to EOF).
    pub well_formed: bool,
}

/// Scan a single value expression starting at `start`, returning where it
/// ends.
///
/// A value ends at the first newline encountered while bracket/paren/brace
/// depth is zero and we are not inside a quoted string. This lets values
/// span multiple physical lines when they contain either a raw newline
/// inside a string, or an array/dictionary/constructor call that itself
/// wraps across lines - while still terminating a plain scalar value (a
/// number, bare identifier, or single-line string) right at its line's end.
pub fn scan_value_span(bytes: &[u8], start: usize) -> ValueScan {
    let mut i = start;
    let mut depth: i64 = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => match scan_string(bytes, i) {
                Ok(end) => i = end,
                Err(eof) => {
                    return ValueScan {
                        end: eof,
                        well_formed: false,
                    }
                }
            },
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                i += 1;
                // A stray closing bracket at depth 0 doesn't terminate the
                // value scan by itself; only a following top-level newline
                // does. Clamp to avoid depth going arbitrarily negative on
                // malformed input.
                if depth < 0 {
                    depth = 0;
                }
            }
            b'\n' if depth == 0 => {
                return ValueScan {
                    end: i,
                    well_formed: true,
                };
            }
            _ => i += 1,
        }
    }
    ValueScan {
        end: bytes.len(),
        well_formed: depth == 0,
    }
}

/// Whether `b` may start a property/attribute key.
pub fn is_key_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Whether `b` may continue a property/attribute key. Godot property keys
/// are path-like (`tracks/0/path`, `metadata/_edit_lock`).
pub fn is_key_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'.' | b':' | b'-')
}

/// Scan a key starting at `pos`. Returns the end index, or `None` if `pos`
/// does not begin a valid key.
pub fn scan_key(bytes: &[u8], pos: usize) -> Option<usize> {
    if pos >= bytes.len() || !is_key_start(bytes[pos]) {
        return None;
    }
    let mut i = pos + 1;
    while i < bytes.len() && is_key_continue(bytes[i]) {
        i += 1;
    }
    Some(i)
}

/// Scan a bracketed section header starting at `bytes[open] == b'['`,
/// matching nested brackets and quoted strings (attribute values inside a
/// header can themselves be strings or arrays, e.g. `groups=["a", "b"]`).
///
/// Returns `Ok(end)` where `end` is the index just past the matching `]`,
/// or `Err(bytes.len())` if it's never closed before EOF.
pub fn scan_header(bytes: &[u8], open: usize) -> Result<usize, usize> {
    debug_assert_eq!(bytes.get(open), Some(&b'['));
    let mut i = open;
    let mut depth: i64 = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => match scan_string(bytes, i) {
                Ok(end) => i = end,
                Err(eof) => return Err(eof),
            },
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => i += 1,
        }
    }
    Err(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_end_with_and_without_trailing_newline() {
        assert_eq!(find_line_end(b"abc\ndef", 0), 4);
        assert_eq!(find_line_end(b"abc", 0), 3);
    }

    #[test]
    fn string_scan_handles_escapes_and_embedded_newlines() {
        let s = b"\"a\\\"b\\\\c\nd\"";
        let end = scan_string(s, 0).unwrap();
        assert_eq!(end, s.len());
    }

    #[test]
    fn string_scan_unterminated_reports_eof() {
        let s = b"\"abc";
        assert_eq!(scan_string(s, 0), Err(4));
    }

    #[test]
    fn header_scan_handles_nested_brackets() {
        let s = b"[node name=\"A\" groups=[\"x\", \"y\"]]\nrest";
        let end = scan_header(s, 0).unwrap();
        assert_eq!(&s[..end], &b"[node name=\"A\" groups=[\"x\", \"y\"]]"[..]);
    }
}
