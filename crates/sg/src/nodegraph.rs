//! Shared node-graph reconstruction from a document's flat `[node ...]`
//! sections: resolving each node's root-relative path from its own
//! `name`/`parent` attributes (independent of file order), plus the
//! parent/child edges, root(s), and orphans that fall out of that
//! resolution.
//!
//! [`crate::rules`] (structural checks: node ordering, orphan/multiple-
//! root detection, connection-endpoint resolution) and [`crate::i18n`]
//! (node path / screen resolution for extracted strings) both need to
//! agree on exactly what a node's path is and whether it is "instanced" -
//! this lives in exactly one place so they always do.

use std::collections::HashMap;

use scenegraph_core::SectionInfo;

pub(crate) fn attr_str<'a>(section: &'a SectionInfo, key: &str) -> Option<&'a str> {
    section
        .attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
}

/// Whether `section` carries an attribute named `key` at all, regardless
/// of its value's type. Used for `instance`/`instance_placeholder`, whose
/// values are not plain strings (`instance=ExtResource(...)`), so
/// [`attr_str`] can't be used to detect their mere presence.
pub(crate) fn has_attr(section: &SectionInfo, key: &str) -> bool {
    section.attrs.iter().any(|(k, _)| k == key)
}

/// Find the raw (unparsed, trimmed) value text of the first property
/// named `key` in a node section's body (`section.properties`), if any.
/// Unlike header attributes (already parsed into a [`scenegraph_core::Value`]
/// by `Document::sections`), property values stay as unparsed text - see
/// [`scenegraph_core::PropertyInfo`] - so a caller that needs a typed value
/// parses `raw_value` itself, e.g. via [`scenegraph_core::parse_complete`].
/// Used by [`crate::i18n::budget`] to read control-geometry properties
/// (`custom_minimum_size`, `offset_left`, `anchor_left`, ...) off the same
/// per-node walk `sg i18n extract` already does.
pub(crate) fn property_raw<'a>(section: &'a SectionInfo, key: &str) -> Option<&'a str> {
    section
        .properties
        .iter()
        .find(|p| p.key == key)
        .map(|p| p.raw_value.as_str())
}

/// A node section is "instanced" when it stands in for the root of a
/// scene this file cannot see into: either a full instantiation
/// (`instance=ExtResource(...)`) or an editor instance placeholder
/// (`instance_placeholder="res://..."`). Either way, anything declared
/// underneath it in the *instanced* scene is invisible here.
pub(crate) fn is_instanced(section: &SectionInfo) -> bool {
    has_attr(section, "instance") || has_attr(section, "instance_placeholder")
}

/// A node's fully-qualified tree path, computed purely from its own
/// `name`/`parent` attributes - independent of file order or of any other
/// node having been seen yet. This is what makes parent-child resolution
/// order-independent: a node's path is not built incrementally while
/// walking the file, it is a pure function of its own header.
pub(crate) fn node_full_path(name: &str, parent_attr: Option<&str>) -> String {
    match parent_attr {
        None => ".".to_string(),
        Some(".") => name.to_string(),
        Some(p) => format!("{p}/{name}"),
    }
}

pub(crate) fn section_indices_of<'a>(sections: &'a [SectionInfo], kind: &'a str) -> Vec<usize> {
    sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == kind)
        .map(|(i, _)| i)
        .collect()
}

pub(crate) struct NodeGraph {
    /// All node section indices, in file order.
    pub node_indices: Vec<usize>,
    /// index -> resolved parent index (`None` for the root). Only
    /// populated for nodes whose parent resolved.
    pub parent_of: HashMap<usize, usize>,
    pub roots: Vec<usize>,
    pub orphans: Vec<(usize, String)>, // (node index, unresolved parent path)
    /// Every node's own root-relative path (see [`node_full_path`]) -> its
    /// section index. This is the full set of paths *declared* in this
    /// file, independent of whether the node's own `parent=` attribute
    /// happens to resolve to anything (an orphan still declares its own
    /// path just fine - see [`NodeGraph::orphans`] for that separate
    /// concern). Used by the `broken-connection-node-path` rule to
    /// resolve `[connection]` `from=`/`to=` targets, and by `sg i18n` to
    /// attach a node's declared path to whatever it extracts from that
    /// node.
    pub path_to_index: HashMap<String, usize>,
}

pub(crate) fn build_node_graph(sections: &[SectionInfo]) -> NodeGraph {
    let node_indices = section_indices_of(sections, "node");
    let mut path_to_index: HashMap<String, usize> = HashMap::new();
    let mut roots = Vec::new();
    for &i in &node_indices {
        let s = &sections[i];
        let name = attr_str(s, "name").unwrap_or("");
        let parent_attr = attr_str(s, "parent");
        if parent_attr.is_none() {
            roots.push(i);
        }
        path_to_index.insert(node_full_path(name, parent_attr), i);
    }

    let mut parent_of = HashMap::new();
    let mut orphans = Vec::new();
    for &i in &node_indices {
        let s = &sections[i];
        let Some(parent_attr) = attr_str(s, "parent") else {
            continue; // root, no parent to resolve
        };
        match path_to_index.get(parent_attr) {
            Some(&p) if p != i => {
                parent_of.insert(i, p);
            }
            _ => orphans.push((i, parent_attr.to_string())),
        }
    }

    NodeGraph {
        node_indices,
        parent_of,
        roots,
        orphans,
        path_to_index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scenegraph_core::Document;

    #[test]
    fn property_raw_finds_a_body_property_by_key() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "custom_minimum_size = Vector2(80, 32)\n",
        );
        let doc = Document::parse(src).unwrap();
        let sections = doc.sections();
        assert_eq!(
            property_raw(&sections[1], "custom_minimum_size"),
            Some("Vector2(80, 32)")
        );
    }

    #[test]
    fn property_raw_returns_none_for_a_missing_key() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Control\"]\n",
            "text = \"Hi\"\n",
        );
        let doc = Document::parse(src).unwrap();
        let sections = doc.sections();
        assert_eq!(property_raw(&sections[1], "custom_minimum_size"), None);
    }
}
