//! The mutation layer: surgical, span-exact edits to a [`Document`]'s
//! source text.
//!
//! Every edit constructor here (`edit_header_attr`, `edit_delete_section`,
//! `edit_reorder_sections`) returns one or more [`Edit`] values describing
//! *only* the byte range that actually needs to change and the text that
//! should replace it - never a re-render of the whole document. This is
//! what lets `sg fix` uphold scenegraph-core's core promise even while
//! mutating: everything outside the sections a fix actually touches stays
//! byte-for-byte identical, because it is never copied through any
//! model - it is simply never included in any [`Edit`]'s span.
//!
//! [`Document::apply_edits`] takes a batch of edits (typically several,
//! computed independently against the same unmodified document - e.g. one
//! `sg fix` run rewriting `load_steps`, deleting unused resources, and
//! reordering out-of-order sections all at once) and applies them in a
//! single left-to-right pass, then re-scans the result into a fresh
//! [`Document`].

use crate::document::Document;
use crate::raw;
use crate::span::Span;
use crate::value;

/// A single replacement: the bytes in `span` (relative to the document's
/// source *at the time the edit was computed*) are replaced with
/// `replacement`. A span with `start == end` is a pure insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

impl Document {
    /// Compute an edit that rewrites section `section_index`'s header
    /// attribute `key` to `new_raw_value` (raw, unquoted-if-numeric source
    /// text - e.g. `"7"` for an integer attribute).
    ///
    /// If the attribute already exists, the edit's span covers *only* its
    /// value text (the header's kind, other attributes, brackets, and
    /// whitespace are all left untouched). If the attribute is absent, the
    /// edit inserts `" key=new_raw_value"` immediately before the header's
    /// closing `]`.
    ///
    /// Returns `None` if `section_index` is out of range or the section's
    /// header cannot be structurally parsed.
    pub fn edit_header_attr(&self, section_index: usize, key: &str, new_raw_value: &str) -> Option<Edit> {
        let section = self.raw.sections.get(section_index)?;
        let inner_text = &self.source[section.inner_span.clone()];
        let spanned = value::parse_header_inner_spanned(inner_text).ok()?;
        match spanned.attrs.iter().find(|(k, _, _)| k == key) {
            Some((_, _, rel_span)) => {
                let start = section.inner_span.start + rel_span.start;
                let end = section.inner_span.start + rel_span.end;
                Some(Edit {
                    span: start..end,
                    replacement: new_raw_value.to_string(),
                })
            }
            None => {
                let insert_at = section.inner_span.end;
                Some(Edit {
                    span: insert_at..insert_at,
                    replacement: format!(" {key}={new_raw_value}"),
                })
            }
        }
    }

    /// Compute an edit that deletes section `section_index` entirely -
    /// its header line and every body line belonging to it (which, per
    /// [`crate::raw::scan`], already includes any trailing blank line(s)
    /// up to the next section header, so deleting it never leaves a
    /// double-blank artifact behind).
    ///
    /// Returns `None` if `section_index` is out of range.
    pub fn edit_delete_section(&self, section_index: usize) -> Option<Edit> {
        let section = self.raw.sections.get(section_index)?;
        Some(Edit {
            span: section_full_span(section),
            replacement: String::new(),
        })
    }

    /// Compute edits that reorder a set of sections among themselves,
    /// without touching anything else in the document.
    ///
    /// `section_indices` names the sections participating in the reorder
    /// (in any order); `new_order` is a permutation of the same set
    /// specifying, in file order, which section's *content* should occupy
    /// each of their byte-range "slots". For example, if `section_indices`
    /// sorted is `[2, 5, 7]` and `new_order` is `[7, 2, 5]`, the section
    /// currently at index 2's slot receives section 7's full text, index
    /// 5's slot receives section 2's text, and index 7's slot receives
    /// section 5's text. Each section's header + body is moved verbatim;
    /// no byte of any moved section's content is altered, and nothing
    /// outside the participating slots is touched - including whatever
    /// non-participating sections happen to sit between them.
    ///
    /// Returns `None` if `new_order` is not a permutation of
    /// `section_indices`, or any index is out of range.
    pub fn edit_reorder_sections(&self, section_indices: &[usize], new_order: &[usize]) -> Option<Vec<Edit>> {
        if section_indices.len() != new_order.len() {
            return None;
        }
        let mut sorted_indices = section_indices.to_vec();
        sorted_indices.sort_unstable();
        let mut sorted_order = new_order.to_vec();
        sorted_order.sort_unstable();
        if sorted_indices != sorted_order {
            return None;
        }
        if sorted_indices.windows(2).any(|w| w[0] == w[1]) {
            // Duplicate indices would make "slot" and "content" ambiguous.
            return None;
        }

        let mut edits = Vec::with_capacity(sorted_indices.len());
        for (&slot_index, &content_index) in sorted_indices.iter().zip(new_order.iter()) {
            let slot_section = self.raw.sections.get(slot_index)?;
            let content_section = self.raw.sections.get(content_index)?;
            let content_span = section_full_span(content_section);
            edits.push(Edit {
                span: section_full_span(slot_section),
                replacement: self.source[content_span].to_string(),
            });
        }
        Some(edits)
    }

    /// Apply a batch of edits - computed via the methods above, all
    /// against *this* document's current spans - in a single pass, and
    /// return the resulting document. Edits are applied left to right in
    /// span order; every byte not covered by any edit's span is copied
    /// through unchanged.
    ///
    /// # Panics
    ///
    /// Panics if any two edits' spans overlap. Every edit constructor
    /// above only ever produces spans confined to the section(s) it was
    /// asked to touch, so a well-formed batch (e.g. one edit per section,
    /// never targeting the same section twice) never overlaps; this is a
    /// programming-error guard, not a condition callers are expected to
    /// need to handle at runtime.
    pub fn apply_edits(&self, mut edits: Vec<Edit>) -> Document {
        edits.sort_by_key(|e| e.span.start);
        let mut out = String::with_capacity(self.source.len());
        let mut cursor = 0usize;
        for edit in &edits {
            assert!(
                edit.span.start >= cursor,
                "scenegraph_core::Document::apply_edits: overlapping edit spans"
            );
            out.push_str(&self.source[cursor..edit.span.start]);
            out.push_str(&edit.replacement);
            cursor = edit.span.end;
        }
        out.push_str(&self.source[cursor..]);

        let (raw, _diagnostics) = raw::scan(&out);
        Document { source: out, raw }
    }
}

/// The full byte span of a section: its header line through the last of
/// its body lines (or just the header line if it has no body), i.e.
/// everything that must move or disappear together when the section is
/// reordered or deleted.
fn section_full_span(section: &raw::SectionChunk) -> Span {
    let end = section
        .body
        .last()
        .map(|line| line.full_span().end)
        .unwrap_or(section.header_span.end);
    section.header_span.start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = concat!(
        "[gd_scene load_steps=3 format=3 uid=\"uid://abc123\"]\n",
        "\n",
        "[ext_resource type=\"Texture2D\" path=\"res://icon.svg\" id=\"1_abc\"]\n",
        "\n",
        "[sub_resource type=\"CapsuleShape2D\" id=\"Cap_1\"]\n",
        "radius = 8.0\n",
        "\n",
        "[node name=\"Main\" type=\"Node2D\"]\n",
    );

    #[test]
    fn header_attr_edit_touches_only_the_value_bytes() {
        let doc = Document::parse(BASIC).unwrap();
        let edit = doc.edit_header_attr(0, "load_steps", "7").unwrap();
        let fixed = doc.apply_edits(vec![edit]);
        let expected = BASIC.replace("load_steps=3", "load_steps=7");
        assert_eq!(fixed.serialize(), expected);
        // Confirm only the digit changed: same length, single differing byte.
        assert_eq!(fixed.serialize().len(), BASIC.len());
        let diffs: Vec<_> = fixed
            .serialize()
            .bytes()
            .zip(BASIC.bytes())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .collect();
        assert_eq!(diffs.len(), 1);
    }

    #[test]
    fn header_attr_edit_inserts_when_attribute_is_absent() {
        let src = "[gd_scene format=3]\n\n[node name=\"Main\" type=\"Node2D\"]\n";
        let doc = Document::parse(src).unwrap();
        let edit = doc.edit_header_attr(0, "load_steps", "1").unwrap();
        let fixed = doc.apply_edits(vec![edit]);
        assert_eq!(
            fixed.serialize(),
            "[gd_scene format=3 load_steps=1]\n\n[node name=\"Main\" type=\"Node2D\"]\n"
        );
    }

    #[test]
    fn delete_section_removes_header_body_and_trailing_blank_only() {
        let doc = Document::parse(BASIC).unwrap();
        // Section 1 is the ext_resource; deleting it must not disturb the
        // gd_scene header, the sub_resource, or the node.
        let edit = doc.edit_delete_section(1).unwrap();
        let fixed = doc.apply_edits(vec![edit]);
        let expected = concat!(
            "[gd_scene load_steps=3 format=3 uid=\"uid://abc123\"]\n",
            "\n",
            "[sub_resource type=\"CapsuleShape2D\" id=\"Cap_1\"]\n",
            "radius = 8.0\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
        );
        assert_eq!(fixed.serialize(), expected);
    }

    #[test]
    fn reorder_sections_swaps_content_leaving_slots_and_gaps_untouched() {
        let src = concat!(
            "[gd_scene load_steps=3 format=3]\n",
            "\n",
            "[sub_resource type=\"A\" id=\"a\"]\n",
            "dep = SubResource(\"b\")\n",
            "\n",
            "[sub_resource type=\"B\" id=\"b\"]\n",
            "value = 1\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        // Sections 1 and 2 are the two sub_resources; "a" (index 1) depends
        // on "b" (index 2), so "b" must come first.
        let edits = doc.edit_reorder_sections(&[1, 2], &[2, 1]).unwrap();
        let fixed = doc.apply_edits(edits);
        let expected = concat!(
            "[gd_scene load_steps=3 format=3]\n",
            "\n",
            "[sub_resource type=\"B\" id=\"b\"]\n",
            "value = 1\n",
            "\n",
            "[sub_resource type=\"A\" id=\"a\"]\n",
            "dep = SubResource(\"b\")\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
        );
        assert_eq!(fixed.serialize(), expected);
    }

    #[test]
    fn reorder_sections_rejects_non_permutation() {
        let doc = Document::parse(BASIC).unwrap();
        assert!(doc.edit_reorder_sections(&[1, 2], &[1, 1]).is_none());
        assert!(doc.edit_reorder_sections(&[1, 2], &[1, 99]).is_none());
    }

    #[test]
    fn combined_batch_of_edits_applies_in_one_pass() {
        let doc = Document::parse(BASIC).unwrap();
        let load_steps_edit = doc.edit_header_attr(0, "load_steps", "2").unwrap();
        let delete_edit = doc.edit_delete_section(1).unwrap();
        let fixed = doc.apply_edits(vec![load_steps_edit, delete_edit]);
        let expected = concat!(
            "[gd_scene load_steps=2 format=3 uid=\"uid://abc123\"]\n",
            "\n",
            "[sub_resource type=\"CapsuleShape2D\" id=\"Cap_1\"]\n",
            "radius = 8.0\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
        );
        assert_eq!(fixed.serialize(), expected);
    }

    #[test]
    #[should_panic(expected = "overlapping edit spans")]
    fn apply_edits_panics_on_overlap() {
        let doc = Document::parse(BASIC).unwrap();
        let e1 = doc.edit_delete_section(1).unwrap();
        let e2 = doc.edit_header_attr(1, "id", "\"x\"").unwrap();
        doc.apply_edits(vec![e1, e2]);
    }
}
