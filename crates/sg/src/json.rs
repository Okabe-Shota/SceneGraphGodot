//! Minimal hand-written JSON serialization for `sg check --json`. Kept
//! dependency-free (no serde) since the shape is tiny and fixed; string
//! escaping is the only part worth being careful about.

use std::path::Path;

use crate::rules::Issue;

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn issue_json(file: &Path, issue: &Issue) -> String {
    format!(
        "{{\"file\":\"{}\",\"line\":{},\"severity\":\"{}\",\"code\":\"{}\",\"message\":\"{}\",\"fixable\":{}}}",
        escape(&file.display().to_string()),
        issue.line,
        issue.severity.as_str(),
        issue.code,
        escape(&issue.message),
        issue.fixable
    )
}

/// Render `items` (each a file path paired with one issue found in it) as
/// a single JSON array.
pub fn issues_array(items: &[(std::path::PathBuf, Issue)]) -> String {
    let mut out = String::from("[");
    for (idx, (file, issue)) in items.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&issue_json(file, issue));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Severity;
    use std::path::PathBuf;

    #[test]
    fn escapes_quotes_and_control_characters() {
        assert_eq!(escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn produces_a_valid_looking_json_array() {
        let items = vec![(
            PathBuf::from("scene.tscn"),
            Issue {
                code: "load-steps-mismatch",
                severity: Severity::Warning,
                line: 1,
                message: "load_steps is 3 but should be 5".to_string(),
                fixable: true,
            },
        )];
        let json = issues_array(&items);
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        assert!(json.contains("\"code\":\"load-steps-mismatch\""));
        assert!(json.contains("\"fixable\":true"));
    }

    #[test]
    fn empty_items_produce_empty_array() {
        assert_eq!(issues_array(&[]), "[]");
    }
}
