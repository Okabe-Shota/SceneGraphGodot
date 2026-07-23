//! Per-project configuration via `sg.toml`: lets a project turn a rule off
//! entirely or override its reported severity. See README.md,
//! "Configuration (sg.toml)".
//!
//! ## Discovery
//!
//! For a checked file, the nearest ancestor directory containing
//! `sg.toml` provides its configuration - the same nearest-ancestor
//! pattern [`crate::paths::find_project_root`] uses for `project.godot`.
//! No `sg.toml` anywhere above a file means built-in defaults (every rule
//! at its normal severity, exactly like before this feature existed).
//! [`ConfigCache`] caches both the directory-to-config-file resolution and
//! the parsed result of each distinct `sg.toml`, so a run over many files
//! sharing a directory (or an ancestor) only walks the filesystem and
//! parses each `sg.toml` once.
//!
//! ## Dependency note
//!
//! This is the project's one deliberate exception to keeping the
//! dependency footprint small (see README.md, "Workspace layout"). TOML's
//! quoting, comments, and other edge cases are not worth hand-rolling just
//! to avoid one dependency, so this uses the `toml` crate - but only its
//! untyped `toml::Table`/`toml::Value` API, so no `serde` derives are
//! added to any project type.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::rules::{Issue, Severity};

/// Every issue code from [`crate::rules::check`] a user is allowed to
/// configure - exactly the rows of the README "sg check" rules table.
/// `parse-error` and every engine-pass code ([`ENGINE_RULE_CODES`]) are
/// deliberately excluded.
const CONFIGURABLE_RULE_CODES: &[&str] = &[
    "load-steps-mismatch",
    "broken-ext-resource-ref",
    "broken-sub-resource-ref",
    "sub-resource-forward-reference",
    "circular-sub-resource-reference",
    "child-before-parent",
    "orphan-node",
    "multiple-root-nodes",
    "duplicate-node-name",
    "unused-ext-resource",
    "unused-sub-resource",
    "duplicate-ext-resource-id",
    "duplicate-sub-resource-id",
    "missing-ext-resource-path",
    "ext-resource-path-case-mismatch",
    "ext-resource-path-is-directory",
    "broken-connection-node-path",
];

/// Issue codes emitted by `sg check --engine` ([`crate::engine`]). Not
/// configurable this round: engine verification always runs at its
/// built-in severity, regardless of `sg.toml`.
const ENGINE_RULE_CODES: &[&str] = &[
    "engine-load-failed",
    "engine-timeout",
    "engine-project-not-found",
    "engine-run-failed",
];

/// One rule's configured behavior, as written in `sg.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSetting {
    Off,
    Warning,
    Error,
}

/// A parsed, validated `sg.toml`. [`Default`] (no overrides at all) is
/// exactly the built-in-defaults behavior, used both when no `sg.toml`
/// exists and when one exists but has no `[rules]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleConfig {
    overrides: HashMap<&'static str, RuleSetting>,
}

impl RuleConfig {
    /// This rule's configured setting, if `sg.toml` mentions it at all.
    /// `None` means "no override; use the rule's built-in default".
    pub fn setting(&self, code: &str) -> Option<RuleSetting> {
        self.overrides.get(code).copied()
    }

    /// Whether `code` is disabled entirely (must neither be reported by
    /// `sg check` nor repaired by `sg fix`).
    pub fn is_off(&self, code: &str) -> bool {
        self.setting(code) == Some(RuleSetting::Off)
    }

    /// The severity `sg check` should report for an issue with this
    /// `code`, given the rule's own built-in `default_severity`. `None`
    /// means the rule is off and the issue must not be reported at all.
    pub fn effective_severity(&self, code: &str, default_severity: Severity) -> Option<Severity> {
        match self.setting(code) {
            None => Some(default_severity),
            Some(RuleSetting::Off) => None,
            Some(RuleSetting::Warning) => Some(Severity::Warning),
            Some(RuleSetting::Error) => Some(Severity::Error),
        }
    }
}

/// A problem with an `sg.toml` file: malformed TOML, an unknown rule name,
/// an invalid value, or a (not-yet-configurable) engine code. Always
/// carries the offending file's path, so the message can name it
/// unambiguously even when several `sg.toml` files exist in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

fn parse_setting(value: &toml::Value) -> Result<RuleSetting, String> {
    let Some(s) = value.as_str() else {
        return Err(format!(
            "value must be a string (\"off\", \"warning\", or \"error\"), got {}",
            value.type_str()
        ));
    };
    match s {
        "off" => Ok(RuleSetting::Off),
        "warning" => Ok(RuleSetting::Warning),
        "error" => Ok(RuleSetting::Error),
        other => Err(format!(
            "invalid value \"{other}\" (expected \"off\", \"warning\", or \"error\")"
        )),
    }
}

/// Parse and validate `source` (an `sg.toml`'s file contents). `path` is
/// used only to label any [`ConfigError`] - this function never touches
/// the filesystem itself.
pub fn parse_config(source: &str, path: &Path) -> Result<RuleConfig, ConfigError> {
    let err = |message: String| ConfigError {
        path: path.to_path_buf(),
        message,
    };

    let table: toml::Table = source
        .parse()
        .map_err(|e: toml::de::Error| err(format!("invalid TOML: {e}")))?;

    let Some(rules_value) = table.get("rules") else {
        return Ok(RuleConfig::default());
    };
    let rules_table = rules_value
        .as_table()
        .ok_or_else(|| err("\"rules\" must be a table, e.g. [rules]".to_string()))?;

    let mut overrides = HashMap::new();
    for (key, value) in rules_table {
        if let Some(&engine_code) = ENGINE_RULE_CODES.iter().find(|&&c| c == key) {
            return Err(err(format!(
                "\"{engine_code}\" is an engine-pass issue code and is not configurable \
                 (sg check --engine issues always run at their built-in severity)"
            )));
        }
        let Some(&known_code) = CONFIGURABLE_RULE_CODES.iter().find(|&&c| c == key) else {
            return Err(err(format!(
                "unknown rule \"{key}\" in [rules] (not one of sg's issue codes - see README.md's rules table)"
            )));
        };
        let setting = parse_setting(value).map_err(|msg| err(format!("rules.{key}: {msg}")))?;
        overrides.insert(known_code, setting);
    }

    Ok(RuleConfig { overrides })
}

/// Filter and remap the output of [`crate::rules::check`] according to
/// `config`: an issue whose rule is `off` is dropped entirely; one whose
/// rule has a severity override has its `severity` field replaced;
/// everything else (including `parse-error` and every `engine-*` code,
/// neither of which can ever appear as a key in `config`'s overrides -
/// see [`parse_config`]) passes through completely unchanged. `config` is
/// `None` when no `sg.toml` governs the file, in which case `issues`
/// passes through unchanged - the exact behavior from before this feature
/// existed.
pub fn apply_to_issues(issues: Vec<Issue>, config: Option<&RuleConfig>) -> Vec<Issue> {
    let Some(config) = config else {
        return issues;
    };
    issues
        .into_iter()
        .filter_map(|issue| {
            let severity = config.effective_severity(issue.code, issue.severity)?;
            Some(Issue { severity, ..issue })
        })
        .collect()
}

/// Walk upward from `dir` looking for `sg.toml`, returning the first
/// ancestor directory that contains one. Mirrors
/// [`crate::paths::find_project_root`]'s walk exactly (same stopping
/// condition: the filesystem root has no parent), just for a different
/// filename.
fn find_sg_toml(dir: &Path) -> Option<PathBuf> {
    let mut dir = dir.to_path_buf();
    loop {
        let candidate = dir.join("sg.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Per-run cache: avoids re-walking the filesystem for every file in a
/// directory, and avoids re-parsing the same `sg.toml` for every file
/// under it.
#[derive(Default)]
pub struct ConfigCache {
    /// Directory -> the `sg.toml` path that governs it (`None` if no
    /// ancestor has one).
    resolved: HashMap<PathBuf, Option<PathBuf>>,
    /// `sg.toml` path -> its parsed config, or the error found while
    /// parsing it. Keyed separately from `resolved` so that many
    /// directories sharing one `sg.toml` (the common case) only pay the
    /// parse cost once.
    parsed: HashMap<PathBuf, Result<Rc<RuleConfig>, ConfigError>>,
}

impl ConfigCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The config that governs `file`: `Ok(None)` if no `sg.toml` exists
    /// above it (built-in defaults apply), `Ok(Some(_))` with the parsed,
    /// validated config otherwise, `Err` if the governing `sg.toml` failed
    /// to parse or validate.
    pub fn load_for_file(&mut self, file: &Path) -> Result<Option<Rc<RuleConfig>>, ConfigError> {
        let dir = match std::path::absolute(file) {
            Ok(abs) => abs.parent().map(|p| p.to_path_buf()),
            Err(_) => file.parent().map(|p| p.to_path_buf()),
        };
        let Some(dir) = dir else {
            return Ok(None);
        };

        let sg_toml_path = match self.resolved.get(&dir) {
            Some(cached) => cached.clone(),
            None => {
                let found = find_sg_toml(&dir);
                self.resolved.insert(dir, found.clone());
                found
            }
        };

        let Some(sg_toml_path) = sg_toml_path else {
            return Ok(None);
        };

        if let Some(cached) = self.parsed.get(&sg_toml_path) {
            return cached.clone().map(Some);
        }

        let result = match std::fs::read_to_string(&sg_toml_path) {
            Ok(source) => parse_config(&source, &sg_toml_path).map(Rc::new),
            Err(e) => Err(ConfigError {
                path: sg_toml_path.clone(),
                message: format!("failed to read: {e}"),
            }),
        };
        self.parsed.insert(sg_toml_path, result.clone());
        result.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // -----------------------------------------------------------------
    // parse_config
    // -----------------------------------------------------------------

    #[test]
    fn empty_file_is_ok_with_no_overrides() {
        let cfg = parse_config("", Path::new("sg.toml")).unwrap();
        assert_eq!(cfg.setting("unused-ext-resource"), None);
    }

    #[test]
    fn missing_rules_section_is_ok_with_no_overrides() {
        let cfg = parse_config("# just a comment\n", Path::new("sg.toml")).unwrap();
        assert_eq!(cfg.setting("unused-ext-resource"), None);
    }

    #[test]
    fn valid_config_parses_off_and_severity_overrides() {
        let src = concat!(
            "[rules]\n",
            "unused-ext-resource = \"off\"\n",
            "ext-resource-path-case-mismatch = \"error\"\n",
            "load-steps-mismatch = \"warning\"\n",
        );
        let cfg = parse_config(src, Path::new("sg.toml")).unwrap();
        assert_eq!(cfg.setting("unused-ext-resource"), Some(RuleSetting::Off));
        assert!(cfg.is_off("unused-ext-resource"));
        assert_eq!(cfg.setting("ext-resource-path-case-mismatch"), Some(RuleSetting::Error));
        assert_eq!(cfg.setting("load-steps-mismatch"), Some(RuleSetting::Warning));
        // A code never mentioned in [rules] has no override.
        assert_eq!(cfg.setting("orphan-node"), None);
    }

    #[test]
    fn unknown_rule_name_is_rejected() {
        let src = "[rules]\nnot-a-real-rule = \"off\"\n";
        let err = parse_config(src, Path::new("sg.toml")).unwrap_err();
        assert!(err.message.contains("not-a-real-rule"), "{}", err.message);
        assert!(err.message.contains("unknown"), "{}", err.message);
    }

    #[test]
    fn invalid_value_is_rejected() {
        let src = "[rules]\nunused-ext-resource = \"disabled\"\n";
        let err = parse_config(src, Path::new("sg.toml")).unwrap_err();
        assert!(err.message.contains("disabled"), "{}", err.message);
    }

    #[test]
    fn non_string_value_is_rejected() {
        let src = "[rules]\nunused-ext-resource = 1\n";
        let err = parse_config(src, Path::new("sg.toml")).unwrap_err();
        assert!(err.message.contains("unused-ext-resource"), "{}", err.message);
    }

    #[test]
    fn engine_code_is_rejected_with_a_dedicated_message() {
        let src = "[rules]\nengine-load-failed = \"off\"\n";
        let err = parse_config(src, Path::new("sg.toml")).unwrap_err();
        assert!(err.message.contains("engine"), "{}", err.message);
        assert!(err.message.contains("not configurable"), "{}", err.message);
    }

    #[test]
    fn malformed_toml_is_rejected() {
        let src = "this is not [ valid toml\n";
        let err = parse_config(src, Path::new("sg.toml")).unwrap_err();
        assert!(err.message.contains("invalid TOML"), "{}", err.message);
    }

    #[test]
    fn rules_section_that_is_not_a_table_is_rejected() {
        let src = "rules = \"nope\"\n";
        let err = parse_config(src, Path::new("sg.toml")).unwrap_err();
        assert!(err.message.contains("table"), "{}", err.message);
    }

    #[test]
    fn config_error_display_includes_the_path() {
        let src = "[rules]\nnot-a-real-rule = \"off\"\n";
        let err = parse_config(src, Path::new("/some/dir/sg.toml")).unwrap_err();
        let shown = err.to_string();
        assert!(shown.contains("sg.toml"), "{shown}");
        assert!(shown.contains("not-a-real-rule"), "{shown}");
    }

    // -----------------------------------------------------------------
    // effective_severity
    // -----------------------------------------------------------------

    #[test]
    fn effective_severity_is_default_when_unconfigured() {
        let cfg = RuleConfig::default();
        assert_eq!(
            cfg.effective_severity("load-steps-mismatch", Severity::Warning),
            Some(Severity::Warning)
        );
    }

    #[test]
    fn effective_severity_is_none_when_off() {
        let src = "[rules]\nload-steps-mismatch = \"off\"\n";
        let cfg = parse_config(src, Path::new("sg.toml")).unwrap();
        assert_eq!(cfg.effective_severity("load-steps-mismatch", Severity::Warning), None);
    }

    #[test]
    fn effective_severity_reflects_promotion_and_demotion() {
        let src = concat!(
            "[rules]\n",
            "load-steps-mismatch = \"error\"\n",
            "broken-ext-resource-ref = \"warning\"\n",
        );
        let cfg = parse_config(src, Path::new("sg.toml")).unwrap();
        assert_eq!(
            cfg.effective_severity("load-steps-mismatch", Severity::Warning),
            Some(Severity::Error)
        );
        assert_eq!(
            cfg.effective_severity("broken-ext-resource-ref", Severity::Error),
            Some(Severity::Warning)
        );
    }

    // -----------------------------------------------------------------
    // apply_to_issues
    // -----------------------------------------------------------------

    fn sample_issue(code: &'static str, severity: Severity) -> Issue {
        Issue {
            code,
            severity,
            line: 1,
            message: "test issue".to_string(),
            fixable: true,
        }
    }

    #[test]
    fn apply_to_issues_passes_through_unchanged_with_no_config() {
        let issues = vec![sample_issue("unused-ext-resource", Severity::Warning)];
        let out = apply_to_issues(issues.clone(), None);
        assert_eq!(out, issues);
    }

    #[test]
    fn apply_to_issues_drops_an_off_rules_issues() {
        let cfg = parse_config("[rules]\nunused-ext-resource = \"off\"\n", Path::new("sg.toml")).unwrap();
        let issues = vec![
            sample_issue("unused-ext-resource", Severity::Warning),
            sample_issue("orphan-node", Severity::Error),
        ];
        let out = apply_to_issues(issues, Some(&cfg));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "orphan-node");
    }

    #[test]
    fn apply_to_issues_remaps_severity_for_a_promoted_rule() {
        let cfg = parse_config(
            "[rules]\next-resource-path-case-mismatch = \"error\"\n",
            Path::new("sg.toml"),
        )
        .unwrap();
        let issues = vec![sample_issue("ext-resource-path-case-mismatch", Severity::Warning)];
        let out = apply_to_issues(issues, Some(&cfg));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Error);
    }

    #[test]
    fn apply_to_issues_remaps_severity_for_a_demoted_rule() {
        let cfg = parse_config("[rules]\nload-steps-mismatch = \"warning\"\n", Path::new("sg.toml")).unwrap();
        let issues = vec![sample_issue("load-steps-mismatch", Severity::Warning)];
        let out = apply_to_issues(issues, Some(&cfg));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Warning);
    }

    #[test]
    fn apply_to_issues_never_touches_engine_or_parse_error_codes() {
        // Engine/parse-error codes can never appear as keys in a valid
        // config's overrides (parse_config rejects them outright), so an
        // issue carrying one of those codes must always pass through
        // unchanged regardless of what else is configured.
        let cfg = parse_config("[rules]\nunused-ext-resource = \"off\"\n", Path::new("sg.toml")).unwrap();
        let issues = vec![
            sample_issue("engine-load-failed", Severity::Error),
            sample_issue("parse-error", Severity::Error),
        ];
        let out = apply_to_issues(issues.clone(), Some(&cfg));
        assert_eq!(out, issues);
    }

    // -----------------------------------------------------------------
    // ConfigCache: nearest-ancestor discovery with caching
    // -----------------------------------------------------------------

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fresh_temp_dir(label: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sg-config-test-{label}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_sg_toml_several_levels_up() {
        let root = fresh_temp_dir("nearest-ancestor");
        std::fs::write(root.join("sg.toml"), "[rules]\nunused-ext-resource = \"off\"\n").unwrap();
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("scene.tscn");
        std::fs::write(&file, "").unwrap();

        let mut cache = ConfigCache::new();
        let cfg = cache.load_for_file(&file).unwrap().expect("sg.toml should be found");
        assert!(cfg.is_off("unused-ext-resource"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_sg_toml_anywhere_yields_none() {
        let dir = fresh_temp_dir("no-config");
        let file = dir.join("scene.tscn");
        std::fs::write(&file, "").unwrap();

        let mut cache = ConfigCache::new();
        // Barring a pathological sg.toml above the OS temp dir, this finds
        // nothing.
        assert_eq!(cache.load_for_file(&file).unwrap(), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nearer_sg_toml_wins_over_a_farther_one() {
        let root = fresh_temp_dir("nearest-wins");
        std::fs::write(root.join("sg.toml"), "[rules]\nunused-ext-resource = \"off\"\n").unwrap();
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("sg.toml"), "[rules]\nunused-ext-resource = \"error\"\n").unwrap();
        let file = nested.join("scene.tscn");
        std::fs::write(&file, "").unwrap();

        let mut cache = ConfigCache::new();
        let cfg = cache.load_for_file(&file).unwrap().unwrap();
        assert_eq!(cfg.setting("unused-ext-resource"), Some(RuleSetting::Error));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cache_reuses_a_previously_parsed_config_across_files() {
        // Delete the sg.toml after the first lookup; a second lookup for a
        // sibling file must still see the config, proving it came from the
        // cache rather than a fresh read.
        let root = fresh_temp_dir("cache-reuse");
        let sg_toml = root.join("sg.toml");
        std::fs::write(&sg_toml, "[rules]\nunused-ext-resource = \"off\"\n").unwrap();
        let file_a = root.join("a.tscn");
        let file_b = root.join("b.tscn");
        std::fs::write(&file_a, "").unwrap();
        std::fs::write(&file_b, "").unwrap();

        let mut cache = ConfigCache::new();
        let cfg_a = cache.load_for_file(&file_a).unwrap().unwrap();
        assert!(cfg_a.is_off("unused-ext-resource"));

        std::fs::remove_file(&sg_toml).unwrap();
        let cfg_b = cache
            .load_for_file(&file_b)
            .unwrap()
            .expect("second lookup should hit the cache, not re-walk the (now-missing) file");
        assert!(cfg_b.is_off("unused-ext-resource"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cached_config_error_is_returned_again_without_re_reading() {
        let root = fresh_temp_dir("cache-error-reuse");
        let sg_toml = root.join("sg.toml");
        std::fs::write(&sg_toml, "[rules]\nnot-a-real-rule = \"off\"\n").unwrap();
        let file_a = root.join("a.tscn");
        let file_b = root.join("b.tscn");
        std::fs::write(&file_a, "").unwrap();
        std::fs::write(&file_b, "").unwrap();

        let mut cache = ConfigCache::new();
        let err_a = cache.load_for_file(&file_a).unwrap_err();
        assert!(err_a.message.contains("not-a-real-rule"));

        let err_b = cache.load_for_file(&file_b).unwrap_err();
        assert!(err_b.message.contains("not-a-real-rule"));
        assert_eq!(err_a.path, err_b.path);

        std::fs::remove_dir_all(&root).ok();
    }
}
