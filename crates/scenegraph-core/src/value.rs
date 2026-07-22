//! Parser for Godot's variant literal grammar: the textual syntax used both
//! for section header attribute values (`type="Texture2D"`) and for
//! property body values (`offset = Vector2(1, 2)`).
//!
//! Grammar (informal):
//!
//! ```text
//! value := 'null' | 'true' | 'false' | number | string | '&' string
//!        | '[' (value (',' value)* ','? )? ']'
//!        | '{' (value ':' value (',' value ':' value)* ','? )? '}'
//!        | IDENT '(' (value (',' value)* ','? )? ')'
//! ```
//!
//! The last alternative covers every "constructor-shaped" value Godot
//! writes: `Vector2(...)`, `Color(...)`, `NodePath(...)`, `Object(...)`,
//! `ExtResource(...)`, `SubResource(...)`, all the `Packed*Array(...)`
//! variants, and any future type the format grows - they're all just a
//! bare identifier followed by a parenthesized argument list.

use crate::error::ValueError;

/// A parsed variant literal.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// `&"name"` - a `StringName` literal.
    StringName(String),
    Array(Vec<Value>),
    /// Order-preserving; Godot dictionaries are insertion-ordered.
    Dictionary(Vec<(Value, Value)>),
    /// A bare-identifier constructor call: `Vector2(1, 2)`,
    /// `ExtResource("1_abc")`, `PackedByteArray("...")`, etc.
    Call {
        name: String,
        args: Vec<Value>,
    },
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::StringName(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn call(&self) -> Option<(&str, &[Value])> {
        match self {
            Value::Call { name, args } => Some((name.as_str(), args.as_slice())),
            _ => None,
        }
    }

    /// If this value is `ExtResource("id")` or `ExtResource(id)`, returns
    /// `id` as a string.
    pub fn as_ext_resource_id(&self) -> Option<String> {
        let (name, args) = self.call()?;
        if name != "ExtResource" {
            return None;
        }
        first_id_arg(args)
    }

    /// If this value is `SubResource("id")` or `SubResource(id)`, returns
    /// `id` as a string.
    pub fn as_sub_resource_id(&self) -> Option<String> {
        let (name, args) = self.call()?;
        if name != "SubResource" {
            return None;
        }
        first_id_arg(args)
    }
}

fn first_id_arg(args: &[Value]) -> Option<String> {
    match args.first()? {
        Value::String(s) => Some(s.clone()),
        Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    Amp,
    Eq,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Eof,
}

/// Decode a double-quoted string starting at `text.as_bytes()[start] ==
/// b'"'`. Returns the decoded value and the byte index just past the
/// closing quote. Tolerates literal, unescaped newlines inside the string
/// body.
fn decode_string(text: &str, start: usize) -> Result<(String, usize), ValueError> {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes.get(start), Some(&b'"'));
    let mut out = String::new();
    let mut i = start + 1;
    let mut run_start = i;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                out.push_str(&text[run_start..i]);
                return Ok((out, i + 1));
            }
            b'\\' => {
                out.push_str(&text[run_start..i]);
                if i + 1 >= bytes.len() {
                    return Err(ValueError {
                        offset: i,
                        message: "unterminated escape sequence".into(),
                    });
                }
                let esc = bytes[i + 1];
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'0' => out.push('\0'),
                    b'a' => out.push('\u{7}'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'v' => out.push('\u{b}'),
                    b'u' => {
                        if i + 6 > bytes.len() {
                            return Err(ValueError {
                                offset: i,
                                message: "invalid \\u escape".into(),
                            });
                        }
                        let hex = &text[i + 2..i + 6];
                        let cp = u32::from_str_radix(hex, 16).map_err(|_| ValueError {
                            offset: i,
                            message: "invalid \\u escape".into(),
                        })?;
                        match char::from_u32(cp) {
                            Some(c) => out.push(c),
                            None => {
                                return Err(ValueError {
                                    offset: i,
                                    message: "invalid unicode code point in \\u escape".into(),
                                })
                            }
                        }
                        i += 6;
                        run_start = i;
                        continue;
                    }
                    other => {
                        // Unknown escape: keep the escaped char literally.
                        // This matches Godot's permissive parser rather
                        // than hard-failing on unfamiliar escapes.
                        out.push(other as char);
                    }
                }
                i += 2;
                run_start = i;
            }
            _ => i += 1,
        }
    }
    Err(ValueError {
        offset: bytes.len(),
        message: "unterminated string literal".into(),
    })
}

fn is_number_lead(bytes: &[u8], i: usize) -> bool {
    if bytes[i].is_ascii_digit() {
        return true;
    }
    if bytes[i] == b'-' || bytes[i] == b'+' {
        let rest = &bytes[i + 1..];
        return rest.first().is_some_and(u8::is_ascii_digit)
            || rest.starts_with(b".")
            || rest.starts_with(b"inf")
            || rest.starts_with(b"nan");
    }
    false
}

fn lex_number(text: &str, start: usize) -> Result<(Tok, usize), ValueError> {
    let bytes = text.as_bytes();
    let mut i = start;
    let negative = bytes[i] == b'-';
    if bytes[i] == b'-' || bytes[i] == b'+' {
        i += 1;
    }
    if text[i..].starts_with("inf") {
        i += 3;
        return Ok((Tok::Float(if negative { f64::NEG_INFINITY } else { f64::INFINITY }), i));
    }
    if text[i..].starts_with("nan") {
        i += 3;
        return Ok((Tok::Float(f64::NAN), i));
    }
    if bytes[i..].starts_with(b"0x") || bytes[i..].starts_with(b"0X") {
        let hex_start = i + 2;
        let mut j = hex_start;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        if j == hex_start {
            return Err(ValueError {
                offset: start,
                message: "invalid hexadecimal integer literal".into(),
            });
        }
        let v = i64::from_str_radix(&text[hex_start..j], 16).map_err(|_| ValueError {
            offset: start,
            message: "invalid hexadecimal integer literal".into(),
        })?;
        return Ok((Tok::Int(if negative { -v } else { v }), j));
    }
    let int_part_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let mut is_float = false;
    if i < bytes.len() && bytes[i] == b'.' {
        is_float = true;
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            is_float = true;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    if i == int_part_start && !is_float {
        return Err(ValueError {
            offset: start,
            message: "invalid number literal".into(),
        });
    }
    let text_slice = &text[start..i];
    if is_float {
        let v: f64 = text_slice.parse().map_err(|_| ValueError {
            offset: start,
            message: format!("invalid float literal '{text_slice}'"),
        })?;
        Ok((Tok::Float(v), i))
    } else {
        let v: i64 = text_slice.parse().map_err(|_| ValueError {
            offset: start,
            message: format!("invalid integer literal '{text_slice}'"),
        })?;
        Ok((Tok::Int(v), i))
    }
}

fn tokenize(text: &str) -> Result<Vec<(Tok, usize)>, ValueError> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'(' => {
                out.push((Tok::LParen, i));
                i += 1;
            }
            b')' => {
                out.push((Tok::RParen, i));
                i += 1;
            }
            b'[' => {
                out.push((Tok::LBracket, i));
                i += 1;
            }
            b']' => {
                out.push((Tok::RBracket, i));
                i += 1;
            }
            b'{' => {
                out.push((Tok::LBrace, i));
                i += 1;
            }
            b'}' => {
                out.push((Tok::RBrace, i));
                i += 1;
            }
            b',' => {
                out.push((Tok::Comma, i));
                i += 1;
            }
            b':' => {
                out.push((Tok::Colon, i));
                i += 1;
            }
            b'=' => {
                out.push((Tok::Eq, i));
                i += 1;
            }
            b'&' => {
                out.push((Tok::Amp, i));
                i += 1;
            }
            b'"' => {
                let (s, end) = decode_string(text, i)?;
                out.push((Tok::Str(s), i));
                i = end;
            }
            _ if is_number_lead(bytes, i) => {
                let (tok, end) = lex_number(text, i)?;
                out.push((tok, i));
                i = end;
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                out.push((Tok::Ident(text[start..j].to_string()), start));
                i = j;
            }
            other => {
                return Err(ValueError {
                    offset: i,
                    message: format!("unexpected character '{}'", other as char),
                });
            }
        }
    }
    out.push((Tok::Eof, bytes.len()));
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].0
    }

    fn peek_offset(&self) -> usize {
        self.toks[self.pos].1
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].0.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, want: &Tok) -> Result<(), ValueError> {
        if self.peek() == want {
            self.bump();
            Ok(())
        } else {
            Err(ValueError {
                offset: self.peek_offset(),
                message: format!("expected {want:?}, found {:?}", self.peek()),
            })
        }
    }

    fn parse_value(&mut self) -> Result<Value, ValueError> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.bump();
                match name.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Null),
                    _ => {
                        self.expect(&Tok::LParen)?;
                        let args = self.parse_arg_list(Tok::RParen)?;
                        Ok(Value::Call { name, args })
                    }
                }
            }
            Tok::Int(n) => {
                self.bump();
                Ok(Value::Int(n))
            }
            Tok::Float(n) => {
                self.bump();
                Ok(Value::Float(n))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Value::String(s))
            }
            Tok::Amp => {
                self.bump();
                match self.bump() {
                    Tok::Str(s) => Ok(Value::StringName(s)),
                    other => Err(ValueError {
                        offset: self.peek_offset(),
                        message: format!("expected string after '&', found {other:?}"),
                    }),
                }
            }
            Tok::LBracket => {
                self.bump();
                let items = self.parse_value_list(Tok::RBracket)?;
                Ok(Value::Array(items))
            }
            Tok::LBrace => {
                self.bump();
                let mut items = Vec::new();
                if self.peek() != &Tok::RBrace {
                    loop {
                        let k = self.parse_value()?;
                        self.expect(&Tok::Colon)?;
                        let v = self.parse_value()?;
                        items.push((k, v));
                        if self.peek() == &Tok::Comma {
                            self.bump();
                            if self.peek() == &Tok::RBrace {
                                break;
                            }
                            continue;
                        }
                        break;
                    }
                }
                self.expect(&Tok::RBrace)?;
                Ok(Value::Dictionary(items))
            }
            other => Err(ValueError {
                offset: self.peek_offset(),
                message: format!("unexpected token {other:?}"),
            }),
        }
    }

    fn parse_value_list(&mut self, close: Tok) -> Result<Vec<Value>, ValueError> {
        let mut items = Vec::new();
        if self.peek() != &close {
            loop {
                items.push(self.parse_value()?);
                if self.peek() == &Tok::Comma {
                    self.bump();
                    if self.peek() == &close {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        self.expect(&close)?;
        Ok(items)
    }

    fn parse_arg_list(&mut self, close: Tok) -> Result<Vec<Value>, ValueError> {
        self.parse_value_list(close)
    }
}

/// Parse `text` as exactly one value, requiring the entire (trimmed) input
/// to be consumed.
pub fn parse_complete(text: &str) -> Result<Value, ValueError> {
    let toks = tokenize(text)?;
    let mut p = Parser { toks, pos: 0 };
    let v = p.parse_value()?;
    if p.peek() != &Tok::Eof {
        return Err(ValueError {
            offset: p.peek_offset(),
            message: "trailing content after value".into(),
        });
    }
    Ok(v)
}

/// The parsed content of a section header: `[kind key=value ...]`.
pub struct ParsedHeader {
    pub kind: String,
    pub attrs: Vec<(String, Value)>,
}

/// Parse the inside of a section header (the text between `[` and `]`,
/// exclusive of both brackets): a bare identifier (the section kind)
/// followed by zero or more `key=value` attributes.
pub fn parse_header_inner(inner: &str) -> Result<ParsedHeader, ValueError> {
    let toks = tokenize(inner)?;
    let mut p = Parser { toks, pos: 0 };
    let kind = match p.bump() {
        Tok::Ident(s) => s,
        other => {
            return Err(ValueError {
                offset: 0,
                message: format!("expected section kind, found {other:?}"),
            })
        }
    };
    let mut attrs = Vec::new();
    loop {
        match p.peek().clone() {
            Tok::Eof => break,
            Tok::Ident(key) => {
                p.bump();
                p.expect(&Tok::Eq)?;
                let val = p.parse_value()?;
                attrs.push((key, val));
            }
            other => {
                return Err(ValueError {
                    offset: p.peek_offset(),
                    message: format!("expected attribute name, found {other:?}"),
                })
            }
        }
    }
    Ok(ParsedHeader { kind, attrs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        assert_eq!(parse_complete("true").unwrap(), Value::Bool(true));
        assert_eq!(parse_complete("false").unwrap(), Value::Bool(false));
        assert_eq!(parse_complete("null").unwrap(), Value::Null);
        assert_eq!(parse_complete("42").unwrap(), Value::Int(42));
        assert_eq!(parse_complete("-7").unwrap(), Value::Int(-7));
        assert_eq!(parse_complete("1.5").unwrap(), Value::Float(1.5));
        assert_eq!(parse_complete("-1.5e-3").unwrap(), Value::Float(-1.5e-3));
    }

    #[test]
    fn parses_strings_with_escapes() {
        let v = parse_complete(r#""a\"b\\c\nd""#).unwrap();
        assert_eq!(v, Value::String("a\"b\\c\nd".to_string()));
    }

    #[test]
    fn parses_string_with_raw_embedded_newline() {
        let v = parse_complete("\"line1\nline2\"").unwrap();
        assert_eq!(v, Value::String("line1\nline2".to_string()));
    }

    #[test]
    fn parses_string_name() {
        assert_eq!(
            parse_complete(r#"&"Sprite2D""#).unwrap(),
            Value::StringName("Sprite2D".to_string())
        );
    }

    #[test]
    fn parses_array_and_dictionary() {
        assert_eq!(
            parse_complete(r#"["a", 1, true]"#).unwrap(),
            Value::Array(vec![Value::String("a".into()), Value::Int(1), Value::Bool(true)])
        );
        let d = parse_complete(r#"{"a": 1, "b": 2}"#).unwrap();
        assert_eq!(
            d,
            Value::Dictionary(vec![
                (Value::String("a".into()), Value::Int(1)),
                (Value::String("b".into()), Value::Int(2)),
            ])
        );
    }

    #[test]
    fn parses_constructor_calls_and_refs() {
        assert_eq!(
            parse_complete("Vector2(1, 2)").unwrap(),
            Value::Call {
                name: "Vector2".into(),
                args: vec![Value::Int(1), Value::Int(2)]
            }
        );
        let ext = parse_complete(r#"ExtResource("1_abc")"#).unwrap();
        assert_eq!(ext.as_ext_resource_id().as_deref(), Some("1_abc"));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse_complete("1 2").is_err());
    }

    #[test]
    fn parses_header_attrs_in_order() {
        let h = parse_header_inner(r#"node name="Player" type="CharacterBody2D" index="0""#).unwrap();
        assert_eq!(h.kind, "node");
        assert_eq!(h.attrs.len(), 3);
        assert_eq!(h.attrs[0].0, "name");
        assert_eq!(h.attrs[1].0, "type");
        assert_eq!(h.attrs[2].0, "index");
    }
}
