//! `sg fix`: turns a [`crate::rules::FixPlan`] into a batch of
//! [`scenegraph_core::Edit`]s and applies them.

use std::path::{Path, PathBuf};

use scenegraph_core::{Document, Edit, ParseError};

use crate::diff;
use crate::rules::{self, FixPlan, Issue};

pub struct FixResult {
    pub path: PathBuf,
    /// Issues found in the original file, before fixing.
    pub before: Vec<Issue>,
    /// Issues remaining after fixing (re-derived by re-checking the fixed
    /// document from scratch, never assumed).
    pub after: Vec<Issue>,
    /// Whether the fixed text differs from the input at all.
    pub changed: bool,
    /// Unified diff from the original source to the fixed source; empty
    /// when `changed` is false.
    pub diff: String,
    pub new_source: String,
}

/// Analyze and fix `source` (the file at `path`, used only for
/// diagnostics/diff labeling - this never touches the filesystem). Callers
/// decide whether to write `new_source` back out.
pub fn fix_file(path: &Path, source: &str, keep_unused: bool) -> Result<FixResult, ParseError> {
    let doc = Document::parse(source)?;
    let before = rules::check(&doc, path);
    let plan = rules::plan_fix(&doc, keep_unused);
    let edits = build_edits(&doc, &plan);

    let fixed_doc = if edits.is_empty() { doc } else { doc.apply_edits(edits) };
    let new_source = fixed_doc.serialize();
    let after = rules::check(&fixed_doc, path);
    let changed = new_source != source;
    let diff = diff::unified_diff(&path.display().to_string(), source, &new_source);

    Ok(FixResult {
        path: path.to_path_buf(),
        before,
        after,
        changed,
        diff,
        new_source,
    })
}

fn build_edits(doc: &Document, plan: &FixPlan) -> Vec<Edit> {
    let mut edits = Vec::new();

    for &i in &plan.delete_sections {
        if let Some(e) = doc.edit_delete_section(i) {
            edits.push(e);
        }
    }
    if let Some((positions, order)) = &plan.sub_resource_reorder {
        if let Some(es) = doc.edit_reorder_sections(positions, order) {
            edits.extend(es);
        }
    }
    if let Some((positions, order)) = &plan.node_reorder {
        if let Some(es) = doc.edit_reorder_sections(positions, order) {
            edits.extend(es);
        }
    }
    if let Some(e) = build_load_steps_edit(doc, plan) {
        edits.push(e);
    }

    edits
}

/// `load_steps` must reflect the resource counts that remain *after*
/// `plan.delete_sections` is applied, so this is computed last and
/// independently of `rules::check`'s (pre-deletion) report - a single fix
/// pass must never need a second pass just to re-correct this number.
fn build_load_steps_edit(doc: &Document, plan: &FixPlan) -> Option<Edit> {
    let fd = doc.file_descriptor()?;
    let sections = doc.sections();
    let ext_count = sections
        .iter()
        .enumerate()
        .filter(|(i, s)| s.kind == "ext_resource" && !plan.delete_sections.contains(i))
        .count();
    let sub_count = sections
        .iter()
        .enumerate()
        .filter(|(i, s)| s.kind == "sub_resource" && !plan.delete_sections.contains(i))
        .count();
    let expected = (ext_count + sub_count + 1) as i64;
    let mismatched = match fd.load_steps {
        Some(actual) => actual != expected,
        None => expected > 1,
    };
    if !mismatched {
        return None;
    }
    doc.edit_header_attr(0, "load_steps", &expected.to_string())
}
