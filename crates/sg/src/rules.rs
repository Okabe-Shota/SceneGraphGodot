//! Detection rules shared by `sg check` and `sg fix`.
//!
//! [`check`] reports every problem found in a document's *current* state.
//! [`plan_fix`] independently derives a concrete, mechanical repair plan
//! (which sections to delete, which to reorder and how) for the subset of
//! problems that are safe to fix automatically. Both build on the same
//! private graph/reachability helpers below so the two never disagree
//! about what "forward reference" or "unused" means.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use scenegraph_core::{collect_references, parse_complete, Document, Reference, ReferenceKind, SectionInfo};

use crate::paths::find_project_root;
use crate::respath::{check_res_path, DirCache, PathCheck};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub severity: Severity,
    pub line: usize,
    pub message: String,
    pub fixable: bool,
}

/// A mechanical repair plan: everything `sg fix` needs to build its batch
/// of [`scenegraph_core::Edit`]s. Does *not* include the `load_steps`
/// rewrite - `sg fix` computes that last, from the section counts that
/// remain after `delete_sections` is applied, so a single fix pass never
/// needs a second pass to correct it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FixPlan {
    /// Section indices (unused ext_resource/sub_resource) to delete
    /// entirely.
    pub delete_sections: BTreeSet<usize>,
    /// `(slot_positions, new_order)` for reordering `sub_resource`
    /// sections into dependency order - see
    /// [`scenegraph_core::Document::edit_reorder_sections`]. `None` if no
    /// reorder is needed (or possible).
    pub sub_resource_reorder: Option<(Vec<usize>, Vec<usize>)>,
    /// Same shape as `sub_resource_reorder`, for `node` sections ordered
    /// so every parent precedes its children.
    pub node_reorder: Option<(Vec<usize>, Vec<usize>)>,
}

fn attr_str<'a>(section: &'a SectionInfo, key: &str) -> Option<&'a str> {
    section
        .attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
}

/// Every `ExtResource`/`SubResource` reference appearing anywhere in one
/// section - its header attributes and its body property values.
fn section_references(section: &SectionInfo) -> Vec<Reference> {
    let mut out = Vec::new();
    for (_, v) in &section.attrs {
        collect_references(v, &mut out);
    }
    for p in &section.properties {
        if let Ok(v) = parse_complete(&p.raw_value) {
            collect_references(&v, &mut out);
        }
    }
    out
}

/// Maps `id` -> every section index declaring that id (in file order), for
/// all sections of the given `kind` ("ext_resource" or "sub_resource").
/// More than one entry for an id is a duplicate-id error.
fn declared_ids(sections: &[SectionInfo], kind: &str) -> HashMap<String, Vec<usize>> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in sections.iter().enumerate() {
        if s.kind != kind {
            continue;
        }
        if let Some(id) = attr_str(s, "id") {
            map.entry(id.to_string()).or_default().push(i);
        }
    }
    map
}

fn section_indices_of<'a>(sections: &'a [SectionInfo], kind: &'a str) -> Vec<usize> {
    sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == kind)
        .map(|(i, _)| i)
        .collect()
}

/// A node's fully-qualified tree path, computed purely from its own
/// `name`/`parent` attributes - independent of file order or of any other
/// node having been seen yet. This is what makes parent-child resolution
/// order-independent: a node's path is not built incrementally while
/// walking the file, it is a pure function of its own header.
fn node_full_path(name: &str, parent_attr: Option<&str>) -> String {
    match parent_attr {
        None => ".".to_string(),
        Some(".") => name.to_string(),
        Some(p) => format!("{p}/{name}"),
    }
}

struct NodeGraph {
    /// All node section indices, in file order.
    node_indices: Vec<usize>,
    /// index -> resolved parent index (`None` for the root). Only
    /// populated for nodes whose parent resolved.
    parent_of: HashMap<usize, usize>,
    roots: Vec<usize>,
    orphans: Vec<(usize, String)>, // (node index, unresolved parent path)
    /// Every node's own root-relative path (see [`node_full_path`]) -> its
    /// section index. This is the full set of paths *declared* in this
    /// file, independent of whether the node's own `parent=` attribute
    /// happens to resolve to anything (an orphan still declares its own
    /// path just fine - see rule 4's `orphans` for that separate concern).
    /// Used by the `broken-connection-node-path` rule to resolve
    /// `[connection]` `from=`/`to=` targets.
    path_to_index: HashMap<String, usize>,
}

fn build_node_graph(sections: &[SectionInfo]) -> NodeGraph {
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

/// Whether `section` carries an attribute named `key` at all, regardless
/// of its value's type. Used for `instance`/`instance_placeholder`, whose
/// values are not plain strings (`instance=ExtResource(...)`), so
/// [`attr_str`] can't be used to detect their mere presence.
fn has_attr(section: &SectionInfo, key: &str) -> bool {
    section.attrs.iter().any(|(k, _)| k == key)
}

/// A node section is "instanced" when it stands in for the root of a
/// scene this file cannot see into: either a full instantiation
/// (`instance=ExtResource(...)`) or an editor instance placeholder
/// (`instance_placeholder="res://..."`). Either way, anything declared
/// underneath it in the *instanced* scene is invisible here.
fn is_instanced(section: &SectionInfo) -> bool {
    has_attr(section, "instance") || has_attr(section, "instance_placeholder")
}

/// Every strict ancestor path of `path`, in the same `.` / `Name` /
/// `Parent/Name` notation used throughout this module - `path` itself is
/// never included. The root (`"."`) has no strict prefixes.
///
/// Example: `"Panel/Button/Icon"` -> `["Panel", "Panel/Button"]`.
fn strict_prefixes(path: &str) -> Vec<String> {
    if path == "." {
        return Vec::new();
    }
    let segments: Vec<&str> = path.split('/').collect();
    (1..segments.len()).map(|end| segments[..end].join("/")).collect()
}

/// Conservative bail-out for NodePath syntax this rule does not attempt to
/// reason about: `%unique_name` lookups, `@`-prefixed paths, `..` parent
/// traversal, and `:subpath` property paths can all resolve to something
/// this file's flat node-path set does not literally contain, even when
/// the connection is perfectly valid. See the module-level rule
/// documentation.
fn is_opaque_node_path(path: &str) -> bool {
    path.contains('%') || path.contains('@') || path.contains("..") || path.contains(':')
}

/// Stable topological sort of `nodes` respecting `depends_on` (an edge `i
/// -> j` in `depends_on[i]` means "i depends on j; j must be placed
/// first"). Ties are broken by original index (smallest eligible index
/// first), so an already-valid order is returned unchanged. Nodes that
/// can never become eligible (participate in, or transitively depend on,
/// a cycle) are appended at the end in their original relative order,
/// which guarantees termination and a total ordering even when a cycle
/// makes part of the input unfixable.
fn stable_topo_sort(nodes: &[usize], depends_on: &HashMap<usize, Vec<usize>>) -> (Vec<usize>, HashSet<usize>) {
    let node_set: HashSet<usize> = nodes.iter().copied().collect();
    let mut in_degree: HashMap<usize, usize> = HashMap::new();
    let mut dependents: HashMap<usize, Vec<usize>> = HashMap::new();
    for &n in nodes {
        let deps: Vec<usize> = depends_on
            .get(&n)
            .map(|v| v.iter().copied().filter(|d| node_set.contains(d)).collect())
            .unwrap_or_default();
        in_degree.insert(n, deps.len());
        for d in deps {
            dependents.entry(d).or_default().push(n);
        }
    }

    let mut eligible: BTreeSet<usize> = nodes.iter().copied().filter(|n| in_degree[n] == 0).collect();
    let mut output = Vec::with_capacity(nodes.len());
    let mut placed: HashSet<usize> = HashSet::new();
    while let Some(&n) = eligible.iter().next() {
        eligible.remove(&n);
        output.push(n);
        placed.insert(n);
        if let Some(deps_of_n) = dependents.get(&n) {
            for &d in deps_of_n {
                if let Some(e) = in_degree.get_mut(&d) {
                    *e -= 1;
                    if *e == 0 {
                        eligible.insert(d);
                    }
                }
            }
        }
    }

    let cyclic: HashSet<usize> = nodes.iter().copied().filter(|n| !placed.contains(n)).collect();
    for &n in nodes {
        if cyclic.contains(&n) {
            output.push(n);
        }
    }
    (output, cyclic)
}

/// Builds the sub_resource -> sub_resource dependency graph (edges among
/// `sub_indices` only; a reference to an id outside that set, or to an id
/// that doesn't exist at all, is not represented here - the former
/// because deleted/excluded resources can't participate, the latter
/// because it is reported separately as a broken reference).
fn sub_resource_deps(
    sections: &[SectionInfo],
    sub_indices: &[usize],
    sub_first: &HashMap<String, usize>,
) -> HashMap<usize, Vec<usize>> {
    let node_set: HashSet<usize> = sub_indices.iter().copied().collect();
    let mut deps = HashMap::new();
    for &i in sub_indices {
        let mut ids: BTreeSet<usize> = BTreeSet::new();
        for r in section_references(&sections[i]) {
            if r.kind == ReferenceKind::SubResource {
                if let Some(&j) = sub_first.get(&r.id) {
                    if j != i && node_set.contains(&j) {
                        ids.insert(j);
                    }
                }
            }
        }
        deps.insert(i, ids.into_iter().collect());
    }
    deps
}

/// Reachability from every root section (`node`, `connection`, and
/// `resource` sections) through `sub_resource` references, transitively.
/// Returns the sets of ext_resource / sub_resource ids reachable from
/// some root.
fn compute_used(sections: &[SectionInfo], sub_first: &HashMap<String, usize>) -> (HashSet<String>, HashSet<String>) {
    let mut used_ext = HashSet::new();
    let mut used_sub = HashSet::new();
    let mut queue: VecDeque<Reference> = VecDeque::new();
    for s in sections {
        if matches!(s.kind.as_str(), "node" | "connection" | "resource") {
            queue.extend(section_references(s));
        }
    }
    while let Some(r) = queue.pop_front() {
        match r.kind {
            ReferenceKind::ExtResource => {
                used_ext.insert(r.id);
            }
            ReferenceKind::SubResource => {
                if used_sub.insert(r.id.clone()) {
                    if let Some(&idx) = sub_first.get(&r.id) {
                        queue.extend(section_references(&sections[idx]));
                    }
                }
            }
        }
    }
    (used_ext, used_sub)
}

/// Report every problem currently present in `doc`. `file` is `doc`'s
/// source path on disk - used only by the ext_resource-path-on-disk rule
/// (rule 7 below) to find the file's Godot project root; every other rule
/// only ever looks at ids declared within `doc` itself.
pub fn check(doc: &Document, file: &Path) -> Vec<Issue> {
    let sections = doc.sections();
    let line = |i: usize| doc.section_line(i).unwrap_or(0);
    let mut issues = Vec::new();

    let ext_indices = section_indices_of(&sections, "ext_resource");
    let sub_indices = section_indices_of(&sections, "sub_resource");
    let ext_occurrences = declared_ids(&sections, "ext_resource");
    let sub_occurrences = declared_ids(&sections, "sub_resource");
    let sub_first: HashMap<String, usize> = sub_occurrences.iter().map(|(id, idxs)| (id.clone(), idxs[0])).collect();

    // Rule 6: duplicate ids.
    for (id, idxs) in &ext_occurrences {
        for &dup in &idxs[1..] {
            issues.push(Issue {
                code: "duplicate-ext-resource-id",
                severity: Severity::Error,
                line: line(dup),
                message: format!(
                    "duplicate ext_resource id \"{id}\" (first declared at line {})",
                    line(idxs[0])
                ),
                fixable: false,
            });
        }
    }
    for (id, idxs) in &sub_occurrences {
        for &dup in &idxs[1..] {
            issues.push(Issue {
                code: "duplicate-sub-resource-id",
                severity: Severity::Error,
                line: line(dup),
                message: format!(
                    "duplicate sub_resource id \"{id}\" (first declared at line {})",
                    line(idxs[0])
                ),
                fixable: false,
            });
        }
    }

    // Rule 2: broken references.
    let mut seen_broken: HashSet<(usize, ReferenceKind, String)> = HashSet::new();
    for (i, s) in sections.iter().enumerate() {
        for r in section_references(s) {
            let declared = match r.kind {
                ReferenceKind::ExtResource => ext_occurrences.contains_key(&r.id),
                ReferenceKind::SubResource => sub_occurrences.contains_key(&r.id),
            };
            if declared {
                continue;
            }
            if !seen_broken.insert((i, r.kind, r.id.clone())) {
                continue;
            }
            let (code, kind_name) = match r.kind {
                ReferenceKind::ExtResource => ("broken-ext-resource-ref", "ext_resource"),
                ReferenceKind::SubResource => ("broken-sub-resource-ref", "sub_resource"),
            };
            issues.push(Issue {
                code,
                severity: Severity::Error,
                line: line(i),
                message: format!("reference to undeclared {kind_name} id \"{}\"", r.id),
                fixable: false,
            });
        }
    }

    // Rule 3: sub_resource forward references (+ circular dependencies).
    let deps = sub_resource_deps(&sections, &sub_indices, &sub_first);
    let (_, cyclic) = stable_topo_sort(&sub_indices, &deps);
    for &i in &sub_indices {
        if cyclic.contains(&i) {
            let id = attr_str(&sections[i], "id").unwrap_or("?");
            issues.push(Issue {
                code: "circular-sub-resource-reference",
                severity: Severity::Error,
                line: line(i),
                message: format!("sub_resource \"{id}\" participates in a circular SubResource dependency"),
                fixable: false,
            });
            continue;
        }
        for &j in deps.get(&i).into_iter().flatten() {
            if j > i {
                let id = attr_str(&sections[i], "id").unwrap_or("?");
                let dep_id = attr_str(&sections[j], "id").unwrap_or("?");
                issues.push(Issue {
                    code: "sub-resource-forward-reference",
                    severity: Severity::Warning,
                    line: line(i),
                    message: format!(
                        "sub_resource \"{id}\" references SubResource(\"{dep_id}\") which is declared later, at line {}",
                        line(j)
                    ),
                    fixable: true,
                });
            }
        }
    }

    // Rule 4: node ordering.
    let graph = build_node_graph(&sections);
    if graph.roots.len() > 1 {
        for &extra_root in &graph.roots[1..] {
            let name = attr_str(&sections[extra_root], "name").unwrap_or("");
            issues.push(Issue {
                code: "multiple-root-nodes",
                severity: Severity::Error,
                line: line(extra_root),
                message: format!("node \"{name}\" has no parent, but a root node was already declared"),
                fixable: false,
            });
        }
    }
    for (i, parent_path) in &graph.orphans {
        let name = attr_str(&sections[*i], "name").unwrap_or("");
        issues.push(Issue {
            code: "orphan-node",
            severity: Severity::Error,
            line: line(*i),
            message: format!("node \"{name}\" has parent=\"{parent_path}\" which does not match any node"),
            fixable: false,
        });
    }
    if graph.roots.len() == 1 && graph.orphans.is_empty() {
        for &i in &graph.node_indices {
            if let Some(&parent_idx) = graph.parent_of.get(&i) {
                if i < parent_idx {
                    let name = attr_str(&sections[i], "name").unwrap_or("");
                    let parent_name = attr_str(&sections[parent_idx], "name").unwrap_or("");
                    issues.push(Issue {
                        code: "child-before-parent",
                        severity: Severity::Warning,
                        line: line(i),
                        message: format!(
                            "node \"{name}\" is declared at line {} but its parent \"{parent_name}\" is declared later, at line {}",
                            line(i),
                            line(parent_idx)
                        ),
                        fixable: true,
                    });
                }
            }
        }
    }

    // Rule 8: connection endpoints must resolve to a node declared in this
    // file. Skipped entirely when the file's own root node is itself an
    // instance (an inherited scene, e.g. `[node name="X"
    // instance=ExtResource(...)]` with no `parent=`): its connections may
    // legitimately target nodes declared only in the base scene, which
    // this file never redeclares. `graph.roots.first()` mirrors the
    // "multiple-root-nodes" rule above: the first node with no `parent=`
    // attribute, in file order, is the file's root.
    let root_is_instance = graph.roots.first().is_some_and(|&r| is_instanced(&sections[r]));
    if !root_is_instance {
        let connection_indices = section_indices_of(&sections, "connection");
        for &i in &connection_indices {
            let s = &sections[i];
            for attr_key in ["from", "to"] {
                let Some(path) = attr_str(s, attr_key) else {
                    continue; // missing/non-string attribute: not this rule's job
                };
                if path.is_empty() || is_opaque_node_path(path) {
                    continue;
                }
                if graph.path_to_index.contains_key(path) {
                    continue; // resolves to a declared node (instanced or not)
                }
                let inside_instanced_scene = strict_prefixes(path).iter().any(|prefix| {
                    graph
                        .path_to_index
                        .get(prefix)
                        .is_some_and(|&idx| is_instanced(&sections[idx]))
                });
                if inside_instanced_scene {
                    continue; // may live inside an instanced sub-scene this file cannot see
                }
                issues.push(Issue {
                    code: "broken-connection-node-path",
                    severity: Severity::Error,
                    line: line(i),
                    message: format!(
                        "connection {attr_key}=\"{path}\" does not resolve to any node declared in this file"
                    ),
                    fixable: false,
                });
            }
        }
    }

    // Rule 5: unused resources.
    let (used_ext, used_sub) = compute_used(&sections, &sub_first);
    for &i in &ext_indices {
        if let Some(id) = attr_str(&sections[i], "id") {
            if !used_ext.contains(id) {
                issues.push(Issue {
                    code: "unused-ext-resource",
                    severity: Severity::Warning,
                    line: line(i),
                    message: format!("ext_resource \"{id}\" is never referenced"),
                    fixable: true,
                });
            }
        }
    }
    for &i in &sub_indices {
        if let Some(id) = attr_str(&sections[i], "id") {
            if !used_sub.contains(id) {
                issues.push(Issue {
                    code: "unused-sub-resource",
                    severity: Severity::Warning,
                    line: line(i),
                    message: format!("sub_resource \"{id}\" is never referenced"),
                    fixable: true,
                });
            }
        }
    }

    // Rule 1: load_steps mismatch.
    if let Some(fd) = doc.file_descriptor() {
        let expected = (ext_indices.len() + sub_indices.len() + 1) as i64;
        let mismatched = match fd.load_steps {
            Some(actual) => actual != expected,
            None => expected > 1,
        };
        if mismatched {
            let actual_desc = fd
                .load_steps
                .map(|n| n.to_string())
                .unwrap_or_else(|| "omitted".to_string());
            issues.push(Issue {
                code: "load-steps-mismatch",
                severity: Severity::Warning,
                line: line(0),
                message: format!(
                    "load_steps is {actual_desc} but should be {expected} ({} ext_resource + {} sub_resource + 1)",
                    ext_indices.len(),
                    sub_indices.len()
                ),
                fixable: true,
            });
        }
    }

    // Rule 7: ext_resource path existence and case on disk. Only runs when
    // `file` sits inside a discoverable Godot project (nearest ancestor
    // directory containing `project.godot`) - without that, a `res://`
    // path has nothing to resolve against, and `sg check --engine` already
    // reports `engine-project-not-found` for that case; this rule silently
    // skips the file instead of duplicating that report (see the module
    // doc comment on `find_project_root`, shared with `crate::engine` via
    // `crate::paths`). Only `path` attributes are inspected - `uid`
    // attributes and non-`res://` paths (e.g. `uid://...`) are untouched.
    if let Some(project_root) = find_project_root(file) {
        let mut dir_cache = DirCache::new();
        for &i in &ext_indices {
            let Some(path_attr) = attr_str(&sections[i], "path") else {
                continue;
            };
            let Some(res_relative) = path_attr.strip_prefix("res://") else {
                continue;
            };
            let id = attr_str(&sections[i], "id").unwrap_or("?");
            match check_res_path(&project_root, res_relative, &mut dir_cache) {
                PathCheck::Exact => {}
                PathCheck::CaseMismatch { actual_relative } => {
                    issues.push(Issue {
                        code: "ext-resource-path-case-mismatch",
                        severity: Severity::Warning,
                        line: line(i),
                        message: format!(
                            "ext_resource \"{id}\" path \"{path_attr}\" exists on disk but with \
                             different case (actual: \"res://{actual_relative}\")"
                        ),
                        fixable: false,
                    });
                }
                PathCheck::Missing => {
                    issues.push(Issue {
                        code: "missing-ext-resource-path",
                        severity: Severity::Error,
                        line: line(i),
                        message: format!("ext_resource \"{id}\" path \"{path_attr}\" does not exist on disk"),
                        fixable: false,
                    });
                }
            }
        }
    }

    issues.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| a.code.cmp(b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
    issues
}

/// Derive a concrete repair plan for everything mechanically fixable.
/// `keep_unused` disables rule 5 (unused-resource deletion) entirely, as
/// if that rule found nothing to delete.
pub fn plan_fix(doc: &Document, keep_unused: bool) -> FixPlan {
    let sections = doc.sections();
    let ext_indices = section_indices_of(&sections, "ext_resource");
    let sub_indices = section_indices_of(&sections, "sub_resource");
    let sub_occurrences = declared_ids(&sections, "sub_resource");
    let sub_first: HashMap<String, usize> = sub_occurrences.iter().map(|(id, idxs)| (id.clone(), idxs[0])).collect();

    let mut plan = FixPlan::default();

    // Rule 5 first: deletions change which sub_resource sections still
    // need to participate in rule 3's reorder, and change the counts
    // rule 1's load_steps fix uses (computed by the caller, after this
    // plan).
    if !keep_unused {
        let (used_ext, used_sub) = compute_used(&sections, &sub_first);
        for &i in &ext_indices {
            if let Some(id) = attr_str(&sections[i], "id") {
                if !used_ext.contains(id) {
                    plan.delete_sections.insert(i);
                }
            }
        }
        for &i in &sub_indices {
            if let Some(id) = attr_str(&sections[i], "id") {
                if !used_sub.contains(id) {
                    plan.delete_sections.insert(i);
                }
            }
        }
    }

    // Rule 3: reorder the sub_resource sections that survive deletion.
    let live_sub_indices: Vec<usize> = sub_indices
        .iter()
        .copied()
        .filter(|i| !plan.delete_sections.contains(i))
        .collect();
    let deps = sub_resource_deps(&sections, &live_sub_indices, &sub_first);
    let (order, _cyclic) = stable_topo_sort(&live_sub_indices, &deps);
    if order != live_sub_indices && !live_sub_indices.is_empty() {
        plan.sub_resource_reorder = Some((live_sub_indices, order));
    }

    // Rule 4: reorder node sections so every parent precedes its children.
    // Only attempted when the node set forms one fully-resolvable tree;
    // orphans/multiple roots are unfixable and left untouched.
    let graph = build_node_graph(&sections);
    if graph.roots.len() == 1 && graph.orphans.is_empty() {
        let depends_on: HashMap<usize, Vec<usize>> = graph
            .node_indices
            .iter()
            .filter_map(|&i| graph.parent_of.get(&i).map(|&p| (i, vec![p])))
            .collect();
        let (order, cyclic) = stable_topo_sort(&graph.node_indices, &depends_on);
        // A tree (one parent per node) can never actually cycle; this is
        // just defense in depth against a future bug in graph
        // construction, not a reachable case today.
        if cyclic.is_empty() && order != graph.node_indices && !graph.node_indices.is_empty() {
            plan.node_reorder = Some((graph.node_indices, order));
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use scenegraph_core::Value;

    fn issue_codes(doc: &Document) -> Vec<&'static str> {
        check(doc, Path::new("test.tscn")).iter().map(|i| i.code).collect()
    }

    // -----------------------------------------------------------------
    // strict_prefixes
    // -----------------------------------------------------------------

    #[test]
    fn strict_prefixes_of_root_is_empty() {
        assert!(strict_prefixes(".").is_empty());
    }

    #[test]
    fn strict_prefixes_of_direct_child_is_empty() {
        // "Button" has no ancestor besides the implicit root, which is not
        // spelled out in root-relative notation.
        assert!(strict_prefixes("Button").is_empty());
    }

    #[test]
    fn strict_prefixes_of_nested_path_lists_every_ancestor() {
        assert_eq!(
            strict_prefixes("Panel/Button/Icon"),
            vec!["Panel".to_string(), "Panel/Button".to_string()]
        );
    }

    // -----------------------------------------------------------------
    // is_opaque_node_path
    // -----------------------------------------------------------------

    #[test]
    fn opaque_path_detects_unique_name_syntax() {
        assert!(is_opaque_node_path("%Button"));
        assert!(is_opaque_node_path("Panel/%Button"));
    }

    #[test]
    fn opaque_path_detects_at_prefix() {
        assert!(is_opaque_node_path("@Button@1"));
    }

    #[test]
    fn opaque_path_detects_parent_traversal() {
        assert!(is_opaque_node_path("../Sibling"));
    }

    #[test]
    fn opaque_path_detects_property_subpath() {
        assert!(is_opaque_node_path("Sprite:frame"));
    }

    #[test]
    fn opaque_path_accepts_ordinary_paths() {
        assert!(!is_opaque_node_path("."));
        assert!(!is_opaque_node_path("Button"));
        assert!(!is_opaque_node_path("Panel/Button"));
    }

    // -----------------------------------------------------------------
    // has_attr / is_instanced
    // -----------------------------------------------------------------

    fn section_with_attrs(kind: &str, attrs: Vec<(&str, Value)>) -> SectionInfo {
        SectionInfo {
            kind: kind.to_string(),
            attrs: attrs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            properties: Vec::new(),
        }
    }

    #[test]
    fn is_instanced_true_for_instance_attribute() {
        let s = section_with_attrs(
            "node",
            vec![
                ("name", Value::String("Enemies".into())),
                (
                    "instance",
                    Value::Call {
                        name: "ExtResource".into(),
                        args: vec![Value::String("1_enemy".into())],
                    },
                ),
            ],
        );
        assert!(is_instanced(&s));
    }

    #[test]
    fn is_instanced_true_for_instance_placeholder_attribute() {
        let s = section_with_attrs(
            "node",
            vec![
                ("name", Value::String("Enemies".into())),
                ("instance_placeholder", Value::String("res://enemy.tscn".into())),
            ],
        );
        assert!(is_instanced(&s));
    }

    #[test]
    fn is_instanced_false_for_plain_node() {
        let s = section_with_attrs("node", vec![("name", Value::String("Main".into()))]);
        assert!(!is_instanced(&s));
    }

    // -----------------------------------------------------------------
    // broken-connection-node-path: end-to-end via check()
    // -----------------------------------------------------------------

    #[test]
    fn reports_broken_connection_from_target() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[connection signal=\"pressed\" from=\"Button\" to=\".\" method=\"_on_pressed\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        let issues = check(&doc, Path::new("test.tscn"));
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.code == "broken-connection-node-path")
                .count(),
            1,
            "{issues:?}"
        );
        let issue = issues.iter().find(|i| i.code == "broken-connection-node-path").unwrap();
        assert_eq!(issue.severity, Severity::Error);
        assert!(!issue.fixable);
        assert_eq!(issue.line, 5);
        assert!(issue.message.contains("from=\"Button\""), "{}", issue.message);
    }

    #[test]
    fn reports_broken_connection_to_target() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[connection signal=\"pressed\" from=\".\" to=\"Missing\" method=\"_on_pressed\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        let codes = issue_codes(&doc);
        assert_eq!(codes, vec!["broken-connection-node-path"]);
    }

    #[test]
    fn valid_connections_are_clean() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[node name=\"Button\" type=\"Button\" parent=\".\"]\n",
            "\n",
            "[connection signal=\"pressed\" from=\"Button\" to=\".\" method=\"_on_button_pressed\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        assert!(issue_codes(&doc).is_empty());
    }

    #[test]
    fn connection_into_instanced_child_scene_is_skipped() {
        // "Enemies" is an instanced sub-scene; "Enemies/Slime" is declared
        // only inside that sub-scene, invisible to this file.
        let src = concat!(
            "[gd_scene load_steps=2 format=3]\n",
            "\n",
            "[ext_resource type=\"PackedScene\" path=\"res://enemy.tscn\" id=\"1_enemy\"]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[node name=\"Enemies\" parent=\".\" instance=ExtResource(\"1_enemy\")]\n",
            "\n",
            "[connection signal=\"died\" from=\"Enemies/Slime\" to=\".\" method=\"_on_died\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        assert!(
            issue_codes(&doc).is_empty(),
            "{:?}",
            check(&doc, Path::new("test.tscn"))
        );
    }

    #[test]
    fn connection_referencing_the_instanced_node_itself_is_declared_and_clean() {
        // The path resolves exactly to the instanced node's own declared
        // path - that node IS declared here, so this is fine regardless of
        // its instance= attribute.
        let src = concat!(
            "[gd_scene load_steps=2 format=3]\n",
            "\n",
            "[ext_resource type=\"PackedScene\" path=\"res://enemy.tscn\" id=\"1_enemy\"]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[node name=\"Enemies\" parent=\".\" instance=ExtResource(\"1_enemy\")]\n",
            "\n",
            "[connection signal=\"died\" from=\"Enemies\" to=\".\" method=\"_on_died\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        assert!(issue_codes(&doc).is_empty());
    }

    #[test]
    fn inherited_scene_root_instance_skips_the_whole_rule() {
        // The file's own root is itself an instance (an inherited scene) -
        // even a wildly broken-looking connection target must not be
        // reported, since it may resolve inside the base scene.
        let src = concat!(
            "[gd_scene load_steps=2 format=3]\n",
            "\n",
            "[ext_resource type=\"PackedScene\" path=\"res://base.tscn\" id=\"1_base\"]\n",
            "\n",
            "[node name=\"Derived\" instance=ExtResource(\"1_base\")]\n",
            "\n",
            "[connection signal=\"pressed\" from=\"DoesNotExistAnywhere\" to=\".\" method=\"_on_pressed\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        assert!(issue_codes(&doc).is_empty());
    }

    #[test]
    fn opaque_paths_in_connections_are_skipped() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[connection signal=\"a\" from=\"%Button\" to=\".\" method=\"m1\"]\n",
            "[connection signal=\"b\" from=\"../Sibling\" to=\".\" method=\"m2\"]\n",
            "[connection signal=\"c\" from=\"Node@1\" to=\".\" method=\"m3\"]\n",
            "[connection signal=\"d\" from=\"Sprite:frame\" to=\".\" method=\"m4\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        assert!(issue_codes(&doc).is_empty());
    }

    #[test]
    fn root_reference_via_dot_resolves() {
        let src = concat!(
            "[gd_scene load_steps=1 format=3]\n",
            "\n",
            "[node name=\"Main\" type=\"Node2D\"]\n",
            "\n",
            "[connection signal=\"pressed\" from=\".\" to=\".\" method=\"_on_pressed\"]\n",
        );
        let doc = Document::parse(src).unwrap();
        assert!(issue_codes(&doc).is_empty());
    }

    #[test]
    fn stable_topo_sort_preserves_already_valid_order() {
        let nodes = vec![0, 1, 2];
        let deps = HashMap::new();
        let (order, cyclic) = stable_topo_sort(&nodes, &deps);
        assert_eq!(order, vec![0, 1, 2]);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn stable_topo_sort_moves_dependency_first() {
        let nodes = vec![0, 1, 2];
        let mut deps = HashMap::new();
        deps.insert(0, vec![2]); // 0 depends on 2
        let (order, cyclic) = stable_topo_sort(&nodes, &deps);
        assert_eq!(order, vec![1, 2, 0]);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn stable_topo_sort_detects_cycle_without_looping() {
        let nodes = vec![0, 1];
        let mut deps = HashMap::new();
        deps.insert(0, vec![1]);
        deps.insert(1, vec![0]);
        let (order, cyclic) = stable_topo_sort(&nodes, &deps);
        assert_eq!(order.len(), 2);
        assert_eq!(cyclic, HashSet::from([0, 1]));
    }
}
