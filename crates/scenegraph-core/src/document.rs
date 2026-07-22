//! Public document model: the lossless [`Document`] plus typed, best-effort
//! structural accessors built on top of it.

use crate::error::{Diagnostic, ParseError, TreeError};
use crate::raw::{self, BodyLine, RawDocument, SectionChunk};
use crate::refs::{collect_references, Reference};
use crate::tree::{self, NodeTree};
use crate::value::{self, ParsedHeader, Value};

/// A parsed Godot text resource file (`.tscn` / `.tres`).
///
/// `Document` keeps the original source alongside a structure of byte
/// spans into it. [`Document::serialize`] replays those spans; for any
/// well-formed input it reproduces the input byte-for-byte, because
/// nothing is ever re-rendered from a parsed model - untouched text is
/// always untouched text.
#[derive(Debug, Clone)]
pub struct Document {
    pub(crate) source: String,
    pub(crate) raw: RawDocument,
}

impl Document {
    /// Parse `source` in strict mode: any part of the file that could not
    /// be confidently classified (an unterminated header, an unclosed
    /// string, unbalanced brackets) causes this to return `Err` describing
    /// the first such problem, with a 1-based line/column.
    pub fn parse(source: &str) -> Result<Document, ParseError> {
        let (raw, diagnostics) = raw::scan(source);
        if let Some(first) = diagnostics.into_iter().next() {
            return Err(first.into());
        }
        Ok(Document {
            source: source.to_string(),
            raw,
        })
    }

    /// Parse `source` in tolerant mode: this never fails. Anything that
    /// can't be classified is preserved verbatim as an "unknown" line so
    /// the document still round-trips, and a diagnostic explaining what
    /// was skipped is returned alongside the document.
    pub fn parse_tolerant(source: &str) -> (Document, Vec<Diagnostic>) {
        let (raw, diagnostics) = raw::scan(source);
        (
            Document {
                source: source.to_string(),
                raw,
            },
            diagnostics,
        )
    }

    /// The original source text this document was parsed from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Reconstruct the source text by replaying every recorded span in
    /// order. For a document produced by [`Document::parse`] (which
    /// requires zero diagnostics), this is always exactly equal to the
    /// input text passed to `parse`.
    pub fn serialize(&self) -> String {
        let mut out = String::with_capacity(self.source.len());
        for line in &self.raw.leading {
            out.push_str(&self.source[line.full_span()]);
        }
        for section in &self.raw.sections {
            out.push_str(&self.source[section.header_span.clone()]);
            for line in &section.body {
                out.push_str(&self.source[line.full_span()]);
            }
        }
        out
    }

    /// Number of section headers (`[...]` blocks) in the document.
    pub fn section_count(&self) -> usize {
        self.raw.sections.len()
    }

    /// The 1-based line number of the `index`-th section's header (same
    /// order as [`Document::sections`]). `None` if `index` is out of
    /// range.
    pub fn section_line(&self, index: usize) -> Option<usize> {
        let section = self.raw.sections.get(index)?;
        Some(crate::span::line_of(&self.source, section.header_span.start))
    }

    fn parsed_header(&self, section: &SectionChunk) -> Option<ParsedHeader> {
        let text = &self.source[section.inner_span.clone()];
        value::parse_header_inner(text).ok()
    }

    fn sections_of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = ParsedHeader> + 'a {
        self.raw
            .sections
            .iter()
            .filter_map(move |s| self.parsed_header(s))
            .filter(move |h| h.kind == kind)
    }

    /// The file's descriptor header: `[gd_scene ...]` or `[gd_resource
    /// ...]`. `None` if the document has no sections or its first section
    /// is not a recognized descriptor.
    pub fn file_descriptor(&self) -> Option<FileDescriptor> {
        let first = self.raw.sections.first()?;
        let header = self.parsed_header(first)?;
        let kind = match header.kind.as_str() {
            "gd_scene" => FileKind::Scene,
            "gd_resource" => FileKind::Resource {
                type_name: attr_str(&header.attrs, "type"),
            },
            _ => return None,
        };
        Some(FileDescriptor {
            kind,
            load_steps: attr_int(&header.attrs, "load_steps"),
            format: attr_int(&header.attrs, "format"),
            uid: attr_str(&header.attrs, "uid"),
        })
    }

    /// All `[ext_resource ...]` sections, in file order.
    pub fn ext_resources(&self) -> Vec<ExtResourceInfo> {
        self.sections_of_kind("ext_resource")
            .map(|h| ExtResourceInfo {
                type_name: attr_str(&h.attrs, "type"),
                path: attr_str(&h.attrs, "path"),
                id: attr_str(&h.attrs, "id"),
                uid: attr_str(&h.attrs, "uid"),
            })
            .collect()
    }

    /// All `[sub_resource ...]` sections, in file order.
    pub fn sub_resources(&self) -> Vec<SubResourceInfo> {
        self.sections_of_kind("sub_resource")
            .map(|h| SubResourceInfo {
                type_name: attr_str(&h.attrs, "type"),
                id: attr_str(&h.attrs, "id"),
            })
            .collect()
    }

    /// All `[node ...]` sections, in file order (the order needed to
    /// resolve `parent` paths via [`Document::build_tree`]).
    pub fn nodes(&self) -> Vec<NodeInfo> {
        self.sections_of_kind("node")
            .map(|h| {
                let groups = attr_value(&h.attrs, "groups")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let instance = attr_value(&h.attrs, "instance").and_then(Value::as_ext_resource_id);
                NodeInfo {
                    name: attr_str(&h.attrs, "name").unwrap_or_default(),
                    type_name: attr_str(&h.attrs, "type"),
                    parent: attr_str(&h.attrs, "parent"),
                    instance,
                    groups,
                    index: attr_int(&h.attrs, "index"),
                }
            })
            .collect()
    }

    /// All `[connection ...]` sections, in file order.
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.sections_of_kind("connection")
            .map(|h| ConnectionInfo {
                signal: attr_str(&h.attrs, "signal"),
                from: attr_str(&h.attrs, "from"),
                to: attr_str(&h.attrs, "to"),
                method: attr_str(&h.attrs, "method"),
                binds: attr_value(&h.attrs, "binds")
                    .and_then(Value::as_array)
                    .map(|items| items.to_vec())
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// All `[editable ...]` sections, in file order.
    pub fn editables(&self) -> Vec<EditableInfo> {
        self.sections_of_kind("editable")
            .map(|h| EditableInfo {
                path: attr_str(&h.attrs, "path"),
            })
            .collect()
    }

    /// Reconstruct the node hierarchy from the flat `[node ...]` sections'
    /// `parent` attributes. See [`crate::tree`] for the path resolution
    /// rules.
    pub fn build_tree(&self) -> Result<NodeTree, TreeError> {
        tree::build_tree(&self.nodes())
    }

    /// Every `ExtResource("id")` / `SubResource("id")` reference appearing
    /// anywhere in the document - in section header attributes (e.g.
    /// `instance=ExtResource(...)`) as well as in property body values,
    /// including references nested inside arrays and dictionaries.
    pub fn references(&self) -> Vec<Reference> {
        let mut out = Vec::new();
        for section in &self.raw.sections {
            if let Some(header) = self.parsed_header(section) {
                for (_, v) in &header.attrs {
                    collect_references(v, &mut out);
                }
            }
            for line in &section.body {
                if let BodyLine::Property(p) = line {
                    let text = self.source[p.value_span.clone()].trim();
                    if let Ok(v) = value::parse_complete(text) {
                        collect_references(&v, &mut out);
                    }
                }
            }
        }
        out
    }

    /// Generic, kind-agnostic view of every section in the document: its
    /// header attributes and its body properties (raw key plus trimmed raw
    /// value text). This is a fallback for section kinds not covered by
    /// the typed accessors above (e.g. `[resource]`, or future/unknown
    /// section kinds), and is what `sg fix`/`sg merge` will build on.
    pub fn sections(&self) -> Vec<SectionInfo> {
        self.raw
            .sections
            .iter()
            .map(|s| {
                let header = self.parsed_header(s).unwrap_or(ParsedHeader {
                    kind: String::new(),
                    attrs: Vec::new(),
                });
                let properties = s
                    .body
                    .iter()
                    .filter_map(|line| match line {
                        BodyLine::Property(p) => Some(PropertyInfo {
                            key: self.source[p.key_span.clone()].to_string(),
                            raw_value: self.source[p.value_span.clone()].trim().to_string(),
                        }),
                        _ => None,
                    })
                    .collect();
                SectionInfo {
                    kind: header.kind,
                    attrs: header.attrs,
                    properties,
                }
            })
            .collect()
    }

    /// Summary counts, e.g. for `sg parse`.
    pub fn stats(&self) -> Stats {
        Stats {
            section_count: self.section_count(),
            ext_resource_count: self.ext_resources().len(),
            sub_resource_count: self.sub_resources().len(),
            node_count: self.nodes().len(),
            connection_count: self.connections().len(),
            reference_count: self.references().len(),
        }
    }
}

fn attr_value<'a>(attrs: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn attr_str(attrs: &[(String, Value)], key: &str) -> Option<String> {
    attr_value(attrs, key).and_then(|v| v.as_str().map(str::to_string))
}

fn attr_int(attrs: &[(String, Value)], key: &str) -> Option<i64> {
    attr_value(attrs, key).and_then(|v| match v {
        Value::Int(n) => Some(*n),
        Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    Scene,
    Resource { type_name: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptor {
    pub kind: FileKind,
    pub load_steps: Option<i64>,
    pub format: Option<i64>,
    pub uid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtResourceInfo {
    pub type_name: Option<String>,
    pub path: Option<String>,
    pub id: Option<String>,
    pub uid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubResourceInfo {
    pub type_name: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeInfo {
    pub name: String,
    pub type_name: Option<String>,
    pub parent: Option<String>,
    pub instance: Option<String>,
    pub groups: Vec<String>,
    pub index: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConnectionInfo {
    pub signal: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub method: Option<String>,
    pub binds: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditableInfo {
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PropertyInfo {
    pub key: String,
    /// Trimmed raw value text, exactly as written (not yet interpreted as
    /// a [`Value`]). Use [`crate::value::parse_complete`] to interpret it.
    pub raw_value: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SectionInfo {
    pub kind: String,
    pub attrs: Vec<(String, Value)>,
    pub properties: Vec<PropertyInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub section_count: usize,
    pub ext_resource_count: usize,
    pub sub_resource_count: usize,
    pub node_count: usize,
    pub connection_count: usize,
    pub reference_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = concat!(
        "[gd_scene load_steps=3 format=3 uid=\"uid://abc123\"]\n",
        "\n",
        "[ext_resource type=\"Texture2D\" uid=\"uid://tex1\" path=\"res://icon.svg\" id=\"1_abc\"]\n",
        "\n",
        "[sub_resource type=\"CapsuleShape2D\" id=\"Cap_1\"]\n",
        "radius = 8.0\n",
        "\n",
        "[node name=\"Main\" type=\"Node2D\"]\n",
        "\n",
        "[node name=\"Sprite\" type=\"Sprite2D\" parent=\".\"]\n",
        "texture = ExtResource(\"1_abc\")\n",
        "shape = SubResource(\"Cap_1\")\n",
    );

    #[test]
    fn round_trips_basic_document() {
        let doc = Document::parse(BASIC).unwrap();
        assert_eq!(doc.serialize(), BASIC);
    }

    #[test]
    fn extracts_file_descriptor() {
        let doc = Document::parse(BASIC).unwrap();
        let fd = doc.file_descriptor().unwrap();
        assert_eq!(fd.kind, FileKind::Scene);
        assert_eq!(fd.load_steps, Some(3));
        assert_eq!(fd.format, Some(3));
        assert_eq!(fd.uid.as_deref(), Some("uid://abc123"));
    }

    #[test]
    fn extracts_ext_and_sub_resources() {
        let doc = Document::parse(BASIC).unwrap();
        let ext = doc.ext_resources();
        assert_eq!(ext.len(), 1);
        assert_eq!(ext[0].id.as_deref(), Some("1_abc"));
        assert_eq!(ext[0].path.as_deref(), Some("res://icon.svg"));
        let sub = doc.sub_resources();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].id.as_deref(), Some("Cap_1"));
    }

    #[test]
    fn extracts_nodes_and_builds_tree() {
        let doc = Document::parse(BASIC).unwrap();
        let nodes = doc.nodes();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].parent, None);
        assert_eq!(nodes[1].parent.as_deref(), Some("."));
        let tree = doc.build_tree().unwrap();
        assert_eq!(tree.root_node().name, "Main");
        assert_eq!(tree.children(tree.root).count(), 1);
    }

    #[test]
    fn extracts_references_from_property_values() {
        let doc = Document::parse(BASIC).unwrap();
        let refs = doc.references();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn strict_parse_rejects_broken_header() {
        let src = "[gd_scene format=3\nbroken";
        assert!(Document::parse(src).is_err());
    }

    #[test]
    fn tolerant_parse_recovers_and_still_round_trips() {
        let src = "[gd_scene format=3\nbroken content that never closes";
        let (doc, diags) = Document::parse_tolerant(src);
        assert!(!diags.is_empty());
        assert_eq!(doc.serialize(), src);
    }
}
