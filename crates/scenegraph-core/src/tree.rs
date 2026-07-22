//! Reconstruction of the node hierarchy from `[node ...]` sections' flat,
//! path-based `parent` attributes.
//!
//! Godot encodes the tree as a flat list where each node names its parent
//! by path, using `.` for "the scene root" rule:
//!
//! - The first node has no `parent` attribute at all: it is the root.
//! - A direct child of the root has `parent="."`.
//! - A deeper descendant has `parent="<path from the root, excluding the
//!   root's own name>"`, e.g. `parent="Player/Sprite2D"`.

use std::collections::HashMap;

use crate::document::NodeInfo;
use crate::error::TreeError;

/// One node in the reconstructed tree. Indices refer into
/// [`NodeTree::nodes`].
#[derive(Debug, Clone, PartialEq)]
pub struct TreeNode {
    pub name: String,
    pub type_name: Option<String>,
    pub instance: Option<String>,
    pub groups: Vec<String>,
    pub index: Option<i64>,
    /// Index of this node in the file-order list returned by
    /// [`crate::document::Document::nodes`].
    pub source_index: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

/// An arena-based reconstruction of the scene's node tree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeTree {
    pub nodes: Vec<TreeNode>,
    pub root: usize,
}

impl NodeTree {
    pub fn root_node(&self) -> &TreeNode {
        &self.nodes[self.root]
    }

    pub fn get(&self, idx: usize) -> &TreeNode {
        &self.nodes[idx]
    }

    pub fn children(&self, idx: usize) -> impl Iterator<Item = &TreeNode> {
        self.nodes[idx].children.iter().map(move |&c| &self.nodes[c])
    }
}

pub(crate) fn build_tree(nodes: &[NodeInfo]) -> Result<NodeTree, TreeError> {
    if nodes.is_empty() {
        return Err(TreeError::NoNodes);
    }

    let mut arena: Vec<TreeNode> = Vec::with_capacity(nodes.len());
    let mut path_to_idx: HashMap<String, usize> = HashMap::new();
    let mut root: Option<usize> = None;

    for (source_index, n) in nodes.iter().enumerate() {
        match &n.parent {
            None => {
                if let Some(existing) = root {
                    return Err(TreeError::MultipleRoots {
                        first: arena[existing].name.clone(),
                        second: n.name.clone(),
                    });
                }
                let idx = arena.len();
                arena.push(TreeNode {
                    name: n.name.clone(),
                    type_name: n.type_name.clone(),
                    instance: n.instance.clone(),
                    groups: n.groups.clone(),
                    index: n.index,
                    source_index,
                    parent: None,
                    children: Vec::new(),
                });
                root = Some(idx);
                path_to_idx.insert(".".to_string(), idx);
            }
            Some(parent_path) => {
                let parent_idx = *path_to_idx
                    .get(parent_path.as_str())
                    .ok_or_else(|| TreeError::OrphanNode {
                        name: n.name.clone(),
                        parent: parent_path.clone(),
                    })?;
                let idx = arena.len();
                arena.push(TreeNode {
                    name: n.name.clone(),
                    type_name: n.type_name.clone(),
                    instance: n.instance.clone(),
                    groups: n.groups.clone(),
                    index: n.index,
                    source_index,
                    parent: Some(parent_idx),
                    children: Vec::new(),
                });
                arena[parent_idx].children.push(idx);
                let full_path = if parent_path == "." {
                    n.name.clone()
                } else {
                    format!("{parent_path}/{}", n.name)
                };
                path_to_idx.insert(full_path, idx);
            }
        }
    }

    let root = root.ok_or(TreeError::NoNodes)?;
    Ok(NodeTree { nodes: arena, root })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, parent: Option<&str>) -> NodeInfo {
        NodeInfo {
            name: name.to_string(),
            type_name: None,
            parent: parent.map(str::to_string),
            instance: None,
            groups: Vec::new(),
            index: None,
        }
    }

    #[test]
    fn builds_multi_level_tree() {
        let nodes = vec![
            node("Main", None),
            node("Player", Some(".")),
            node("Sprite2D", Some("Player")),
            node("Hitbox", Some("Player/Sprite2D")),
        ];
        let tree = build_tree(&nodes).unwrap();
        assert_eq!(tree.root_node().name, "Main");
        assert_eq!(
            tree.children(tree.root).map(|c| c.name.clone()).collect::<Vec<_>>(),
            vec!["Player"]
        );
        let player = &tree.nodes[tree.root_node().children[0]];
        assert_eq!(player.children.len(), 1);
        let sprite = &tree.nodes[player.children[0]];
        assert_eq!(sprite.name, "Sprite2D");
        assert_eq!(sprite.children.len(), 1);
        assert_eq!(tree.nodes[sprite.children[0]].name, "Hitbox");
    }

    #[test]
    fn rejects_multiple_roots() {
        let nodes = vec![node("A", None), node("B", None)];
        assert!(matches!(build_tree(&nodes), Err(TreeError::MultipleRoots { .. })));
    }

    #[test]
    fn rejects_orphan_parent_path() {
        let nodes = vec![node("A", None), node("B", Some("DoesNotExist"))];
        assert!(matches!(build_tree(&nodes), Err(TreeError::OrphanNode { .. })));
    }

    #[test]
    fn empty_node_list_is_an_error() {
        assert_eq!(build_tree(&[]), Err(TreeError::NoNodes));
    }
}
