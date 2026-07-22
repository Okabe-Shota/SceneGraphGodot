//! `sg check --engine`: headless-Godot verification that each `.tscn`/
//! `.tres` file actually loads, on top of (never instead of) the static
//! checks in [`crate::rules`]. The static checker is our own parser's
//! self-report; this module makes the running Godot engine itself the
//! final arbiter, since only it can tell whether an `ExtResource` `path`
//! attribute actually resolves to something on disk - `sg check`'s static
//! rules only ever validate ids declared *within* the file, never whether
//! the files those ids point to exist.
//!
//! ## Design
//!
//! 1. For each target file, walk upward from its directory looking for a
//!    `project.godot` ([`find_project_root`]) - that directory is the only
//!    thing that gives a `res://` path ([`to_res_path`]) meaning. A file
//!    with no `project.godot` above it is unrooted and reported as an
//!    `engine-project-not-found` issue instead of being handed to Godot at
//!    all.
//! 2. Files are grouped by project root ([`group_by_project`]) so every
//!    file belonging to the same Godot project is verified by a single
//!    Godot process launch, amortizing engine startup cost; a checkout
//!    that happens to straddle more than one Godot project still gets one
//!    launch per project rather than failing outright.
//! 3. For each group, a small generated GDScript ([`VALIDATOR_SCRIPT`]) is
//!    written to a fresh directory under the OS temp dir (never inside the
//!    project being checked - `sg` must not leave residue in a directory
//!    it was only asked to *check*) and run via `godot --headless --path
//!    <project> --script <that file> -- <res:// paths...>`. The script
//!    loads each path through Godot's own `ResourceLoader` and prints one
//!    machine-readable result line per path - see [`VALIDATOR_SCRIPT`]'s
//!    own doc comment for the exact protocol and why a plain `== null`
//!    check on `ResourceLoader.load()` is not sufficient by itself.
//! 4. The whole invocation runs under a timeout ([`run_with_timeout`]): a
//!    hung Godot process is killed rather than left to block `sg`
//!    indefinitely, and whatever partial stdout it produced before being
//!    killed is still parsed, so files that *did* get a result line before
//!    a hang are still reported precisely - only the ones that never got
//!    one fall back to a generic timeout issue.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::rules::{Issue, Severity};

/// GDScript run inside the target project by [`validate_group`]. Must
/// extend `SceneTree` (a requirement of Godot's `--script` flag for a
/// script meant to run standalone, without a scene).
///
/// For every `res://` path passed as a user argument (after `--`), two
/// independent, engine-authoritative checks are performed - deliberately
/// *not* a reimplementation of `sg`'s own static rules, since the entire
/// point of `--engine` is to defer to Godot's own judgment:
///
/// 1. Every direct dependency Godot's `ResourceLoader.get_dependencies`
///    reports for the file (in practice, its `ext_resource` paths) is
///    checked with `ResourceLoader.exists`. This is the check that catches
///    what `sg check`'s static rules structurally cannot: an
///    `ext_resource` whose `path` attribute is syntactically fine but
///    points at nothing. Verified empirically against
///    `fixtures/engine_project/broken.tscn` (see README.md): Godot's
///    `PackedScene` loader does *not* fail outright for this - it loads
///    the scene anyway with that one dependency left unresolved, so a bare
///    `ResourceLoader.load() == null` check alone misses it entirely. That
///    is why this dependency-existence check exists in addition to, not
///    instead of, the load below.
/// 2. `ResourceLoader.load()` itself returns `null` - catches load
///    failures that aren't a missing direct dependency (e.g. a file Godot
///    considers structurally invalid).
///
/// Output protocol (stdout, tab-separated, one line per input path plus a
/// trailing summary line):
///
/// ```text
/// SG-ENGINE-RESULT\tOK\t<res_path>\t
/// SG-ENGINE-RESULT\tFAIL\t<res_path>\t<reason>
/// SG-ENGINE-DONE\tOK|FAIL
/// ```
///
/// Godot's own startup banner and any `ERROR:`/warning lines it prints
/// while loading go to stdout/stderr as usual and are simply not
/// `SG-ENGINE-RESULT`-prefixed, so [`parse_validator_output`] ignores them
/// without needing to suppress or redirect them.
const VALIDATOR_SCRIPT: &str = r#"extends SceneTree

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	var any_fail := false
	for res_path in args:
		var reasons: Array = []
		var deps: PackedStringArray = ResourceLoader.get_dependencies(res_path)
		for dep in deps:
			if not ResourceLoader.exists(dep):
				reasons.append("missing dependency: %s" % dep)
		var res: Resource = ResourceLoader.load(res_path, "", ResourceLoader.CACHE_MODE_IGNORE)
		if res == null:
			reasons.append("ResourceLoader.load() returned null")
		if reasons.is_empty():
			print("SG-ENGINE-RESULT\tOK\t%s\t" % res_path)
		else:
			var msg: String = "; ".join(reasons)
			msg = msg.replace("\t", " ").replace("\n", " ")
			print("SG-ENGINE-RESULT\tFAIL\t%s\t%s" % [res_path, msg])
			any_fail = true
	print("SG-ENGINE-DONE\t%s" % ("FAIL" if any_fail else "OK"))
	quit(1 if any_fail else 0)
"#;

const GODOT_CANDIDATE_NAMES: &[&str] = &["godot4", "godot"];

// ---------------------------------------------------------------------
// Godot binary discovery
// ---------------------------------------------------------------------

/// Locate the Godot executable to run for `--engine`: `cli_flag` (the
/// `--godot-path` argument) if given, else the `SG_GODOT` environment
/// variable, else `godot4`/`godot` on `PATH`, in that order. Each tier is
/// checked in isolation - an explicit `--godot-path`/`SG_GODOT` that
/// doesn't point at a real file is a hard error (a clear "you told me to
/// use this and it doesn't work" beats silently falling through to a
/// binary the user didn't ask for).
pub fn find_godot_binary(cli_flag: Option<&Path>) -> Result<PathBuf, String> {
    let env_sg_godot = std::env::var("SG_GODOT").ok();
    let path_var = std::env::var_os("PATH");
    resolve_godot_binary(cli_flag, env_sg_godot.as_deref(), path_var.as_deref())
}

/// The pure decision logic behind [`find_godot_binary`], taking its inputs
/// as plain values instead of reading the process environment directly so
/// it can be unit tested without mutating global process state (env vars
/// are process-wide and `cargo test` runs tests in parallel threads of the
/// same process).
fn resolve_godot_binary(
    cli_flag: Option<&Path>,
    env_sg_godot: Option<&str>,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if let Some(p) = cli_flag {
        return if is_executable_file(p) {
            Ok(p.to_path_buf())
        } else {
            Err(format!(
                "--godot-path '{}' does not point to an executable file",
                p.display()
            ))
        };
    }

    if let Some(env_path) = env_sg_godot {
        if !env_path.trim().is_empty() {
            let p = PathBuf::from(env_path);
            return if is_executable_file(&p) {
                Ok(p)
            } else {
                Err(format!(
                    "SG_GODOT is set to '{env_path}' but it does not point to an executable file"
                ))
            };
        }
    }

    if let Some(path_var) = path_var {
        for name in GODOT_CANDIDATE_NAMES {
            if let Some(found) = which_in_path(name, path_var) {
                return Ok(found);
            }
        }
    }

    Err("could not find a Godot executable for --engine. Checked, in order: \
         (1) --godot-path flag: not given; \
         (2) SG_GODOT environment variable: not set; \
         (3) 'godot4' or 'godot' on PATH: not found. \
         Pass --godot-path <path>, set SG_GODOT, or add a Godot 4.x executable \
         named 'godot4' or 'godot' to PATH."
        .to_string())
}

fn which_in_path(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        let base = dir.join(name);
        if let Some(found) = exe_candidate(&base) {
            return Some(found);
        }
    }
    None
}

#[cfg(windows)]
fn exe_candidate(base: &Path) -> Option<PathBuf> {
    if is_executable_file(base) {
        return Some(base.to_path_buf());
    }
    for ext in ["exe", "cmd", "bat"] {
        let candidate = base.with_extension(ext);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn exe_candidate(base: &Path) -> Option<PathBuf> {
    if is_executable_file(base) {
        Some(base.to_path_buf())
    } else {
        None
    }
}

#[cfg(windows)]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------
// Project discovery and res:// resolution
// ---------------------------------------------------------------------

/// Walk upward from `file`'s directory looking for `project.godot`,
/// returning the first ancestor directory that contains one. Resolves
/// `file` to an absolute path first (lexically, via [`std::path::absolute`]
/// - no filesystem access, no symlink resolution) so relative inputs like
///   a bare `scene.tscn` (whose `Path::parent()` would otherwise be the
///   empty path, terminating the walk after a single check) are handled the
///   same as any other input.
pub fn find_project_root(file: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(file).ok()?;
    let mut dir = abs.parent()?.to_path_buf();
    loop {
        if dir.join("project.godot").is_file() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Express `file` as a `res://`-relative path within `project_root`.
/// `None` if `file` is not lexically inside `project_root` at all, or if
/// the relative path would need to leave it (e.g. via a symlink-free `..`
/// component, which can't happen for a true descendant but is guarded
/// against defensively).
pub fn to_res_path(project_root: &Path, file: &Path) -> Option<String> {
    let root_abs = std::path::absolute(project_root).ok()?;
    let file_abs = std::path::absolute(file).ok()?;
    let rel = file_abs.strip_prefix(&root_abs).ok()?;

    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("res://{}", parts.join("/")))
}

/// One Godot project's worth of files to verify in a single engine launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGroup {
    pub project_root: PathBuf,
    /// `(original path, its res:// path within this project)`, in the
    /// order the files were supplied.
    pub files: Vec<(PathBuf, String)>,
}

/// Partition `files` by the Godot project each belongs to
/// ([`find_project_root`]), preserving input order both within each group
/// and across the returned group list (first-seen project first). Files
/// with no discoverable `project.godot` above them (or whose res://
/// path could not be computed) are returned separately rather than
/// dropped, so the caller can still report them.
pub fn group_by_project(files: &[PathBuf]) -> (Vec<ProjectGroup>, Vec<PathBuf>) {
    let mut groups: Vec<ProjectGroup> = Vec::new();
    let mut unrooted = Vec::new();

    for file in files {
        let rooted = find_project_root(file).and_then(|root| to_res_path(&root, file).map(|res_path| (root, res_path)));
        match rooted {
            Some((root, res_path)) => match groups.iter_mut().find(|g| g.project_root == root) {
                Some(group) => group.files.push((file.clone(), res_path)),
                None => groups.push(ProjectGroup {
                    project_root: root,
                    files: vec![(file.clone(), res_path)],
                }),
            },
            None => unrooted.push(file.clone()),
        }
    }

    (groups, unrooted)
}

// ---------------------------------------------------------------------
// Windows process-tree kill helper
// ---------------------------------------------------------------------
//
// `Child::kill()` only terminates the direct child process. On Windows
// that is not enough when the child is itself a wrapper - e.g. `cmd /C
// some-command` - because the wrapper's own subprocess inherits the same
// stdout/stderr pipe handles the wrapper was given. Killing the wrapper
// closes only *its* copy of those handles; the grandchild keeps its own
// copy open, so the pipe never reaches EOF and the reader threads in
// [`run_with_timeout`] block on `read_to_end` until the grandchild exits
// on its own, defeating the timeout entirely. A Windows Job Object with
// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` captures the child and everything
// it spawns, so terminating the job tears down the whole tree at once.
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// An anonymous Job Object that a child process (and, transitively,
    /// anything that process later spawns) can be assigned to, so the
    /// whole tree can be torn down with a single [`Job::terminate`] call.
    /// Also configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` as a
    /// safety net: if this handle is ever dropped without an explicit
    /// `terminate`, Windows kills the tree anyway instead of leaking it.
    pub struct Job(HANDLE);

    impl Job {
        pub fn new() -> io::Result<Self> {
            // SAFETY: `CreateJobObjectW` with null security attributes and
            // no name is a documented way to create an anonymous job
            // object; the returned handle is checked for null below.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `job.0` is the valid job handle created above, and
            // `info` is a validly initialized instance of the struct type
            // `JobObjectExtendedLimitInformation` expects, with a matching
            // size.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }

            Ok(job)
        }

        /// Put `child` into this job, so it (and anything it spawns) is
        /// covered by a later [`Job::terminate`].
        pub fn assign(&self, child: &Child) -> io::Result<()> {
            let process_handle = child.as_raw_handle() as HANDLE;
            // SAFETY: `self.0` is a valid job handle owned by this `Job`,
            // and `process_handle` is the handle of a live child process
            // owned by `child`, borrowed only for the duration of the
            // call.
            let ok = unsafe { AssignProcessToJobObject(self.0, process_handle) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Kill every process currently in the job, not just the one
        /// directly spawned by the caller.
        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: `self.0` is a valid job handle owned by this `Job`.
            let ok = unsafe { TerminateJobObject(self.0, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a valid handle owned solely by this
            // `Job` and not closed anywhere else.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Process execution with a timeout
// ---------------------------------------------------------------------

/// Result of running a process to completion or killing it after
/// `timeout` elapsed. `stdout`/`stderr` hold whatever the process produced
/// before exiting or being killed - never truncated to "nothing" just
/// because a timeout occurred, since a hung Godot process may well have
/// already printed results for some of the files it was asked to check.
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Spawn `command` with piped stdio, poll for completion, and kill it if
/// `timeout` elapses first. Not Godot-specific - a generic building block,
/// which is what makes it unit-testable without Godot (see the `tests`
/// module below).
///
/// Ownership of the `Child` is kept on the calling thread throughout (only
/// its stdout/stderr pipes are handed to reader threads), so killing it on
/// timeout never has to coordinate across threads for anything beyond
/// draining those two pipes.
pub fn run_with_timeout(mut command: Command, timeout: Duration) -> std::io::Result<ProcessOutcome> {
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());
    let mut child = command.spawn()?;

    // Assign the child to a Job Object right away so that if it turns out
    // to be a wrapper (e.g. `cmd /C ...`), everything it spawns is also
    // covered by the timeout kill below - see the `job` module doc comment
    // for why killing just the direct child is not enough on Windows.
    // Best-effort: if job creation or assignment fails, the timeout path
    // below falls back to killing only the direct child.
    #[cfg(windows)]
    let job = job::Job::new().ok();
    #[cfg(windows)]
    if let Some(job) = &job {
        let _ = job.assign(&child);
    }

    let mut stdout_pipe = child.stdout.take().expect("stdout was requested as piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was requested as piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });

    const POLL_INTERVAL: Duration = Duration::from_millis(25);
    let start = Instant::now();
    let mut timed_out = false;
    let status: ExitStatus = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            // Kill the whole process tree first (Windows), then the
            // direct child as a fallback in case the job was never set up.
            #[cfg(windows)]
            if let Some(job) = &job {
                let _ = job.terminate();
            }
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    Ok(ProcessOutcome {
        exit_code: status.code(),
        stdout,
        stderr,
        timed_out,
    })
}

// ---------------------------------------------------------------------
// Validator output
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoadOutcome {
    Ok,
    Fail(String),
}

/// Parse [`VALIDATOR_SCRIPT`]'s stdout protocol into `res:// path ->
/// outcome`. Any line not prefixed with the exact `SG-ENGINE-RESULT`
/// marker (Godot's startup banner, `ERROR:`/warning lines, anything else)
/// is silently ignored rather than tripping up parsing - the marker is
/// deliberately distinctive so this never has to guess.
fn parse_validator_output(stdout: &str) -> HashMap<String, LoadOutcome> {
    let mut results = HashMap::new();
    for line in stdout.lines() {
        let mut fields = line.splitn(4, '\t');
        if fields.next() != Some("SG-ENGINE-RESULT") {
            continue;
        }
        let (Some(status), Some(path)) = (fields.next(), fields.next()) else {
            continue;
        };
        let reason = fields.next().unwrap_or("");
        let outcome = if status == "OK" {
            LoadOutcome::Ok
        } else {
            LoadOutcome::Fail(reason.to_string())
        };
        results.insert(path.to_string(), outcome);
    }
    results
}

// ---------------------------------------------------------------------
// Running the validator against one project group
// ---------------------------------------------------------------------

static SCRIPT_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write [`VALIDATOR_SCRIPT`] to a fresh directory under the OS temp dir
/// (never inside the project being checked - see the module doc comment)
/// and return the script's path. Every call gets its own directory (pid +
/// a process-local counter) so concurrent groups, and concurrent `sg`
/// processes, never collide.
fn write_validator_script() -> std::io::Result<PathBuf> {
    let n = SCRIPT_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("sg-engine-validator-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("sg_validate.gd");
    std::fs::write(&path, VALIDATOR_SCRIPT)?;
    Ok(path)
}

/// Truncate `s` to at most `max_chars` characters, keeping the *tail* (the
/// most recent output is the most relevant for diagnosing a crash/hang)
/// and collapsing newlines so it stays a single display line.
fn tail(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    let char_count = s.chars().count();
    let clipped = if char_count <= max_chars {
        s.to_string()
    } else {
        let start = s.char_indices().rev().nth(max_chars - 1).map(|(i, _)| i).unwrap_or(0);
        format!("...{}", &s[start..])
    };
    clipped.replace(['\n', '\r'], " ")
}

/// Run one headless Godot launch covering every file in `group`, and
/// derive a per-file issue (`None` = loaded cleanly) from its output.
///
/// `Err` is reserved for environment-level failures - the Godot process
/// could not even be started - which the caller surfaces as `sg`'s new
/// exit code `3`, distinct from per-file load failures (issues, exit code
/// `1`).
pub fn validate_group(
    godot_bin: &Path,
    group: &ProjectGroup,
    timeout: Duration,
) -> Result<Vec<(PathBuf, Option<Issue>)>, String> {
    let script_path =
        write_validator_script().map_err(|e| format!("failed to write temporary validator script: {e}"))?;

    let mut command = Command::new(godot_bin);
    command
        .arg("--headless")
        .arg("--path")
        .arg(&group.project_root)
        .arg("--script")
        .arg(&script_path)
        .arg("--");
    for (_, res_path) in &group.files {
        command.arg(res_path);
    }

    let run_result = run_with_timeout(command, timeout);

    if let Some(dir) = script_path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }

    let outcome = run_result.map_err(|e| {
        format!(
            "failed to run Godot ('{}') for project '{}': {e}",
            godot_bin.display(),
            group.project_root.display()
        )
    })?;

    let parsed = parse_validator_output(&outcome.stdout);
    let mut results = Vec::with_capacity(group.files.len());
    for (file, res_path) in &group.files {
        let issue = match parsed.get(res_path) {
            Some(LoadOutcome::Ok) => None,
            Some(LoadOutcome::Fail(reason)) => Some(Issue {
                code: "engine-load-failed",
                severity: Severity::Error,
                line: 1,
                message: format!("Godot failed to load '{res_path}': {reason}"),
                fixable: false,
            }),
            None if outcome.timed_out => Some(Issue {
                code: "engine-timeout",
                severity: Severity::Error,
                line: 1,
                message: format!(
                    "Godot engine validation timed out after {}s (project '{}')",
                    timeout.as_secs(),
                    group.project_root.display()
                ),
                fixable: false,
            }),
            None => Some(Issue {
                code: "engine-run-failed",
                severity: Severity::Error,
                line: 1,
                message: format!(
                    "Godot exited (code {:?}) without reporting a result for '{res_path}'; stderr: {}",
                    outcome.exit_code,
                    tail(&outcome.stderr, 300)
                ),
                fixable: false,
            }),
        };
        results.push((file.clone(), issue));
    }
    Ok(results)
}

// ---------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------

/// Run `--engine` verification for `files` (already filtered to
/// `.tscn`/`.tres` and expanded from any directories by
/// [`crate::paths::collect_target_files`]) and return the issues found,
/// one per file that failed to load plus one per file that couldn't be
/// resolved to any Godot project at all.
///
/// `Err` means an environment-level problem prevented verification from
/// running at all (no Godot binary found, or a group's Godot process
/// could not be started) - the caller maps this to exit code `3`, not `1`,
/// since it says nothing about whether the files themselves are valid.
pub fn run_engine_checks(
    files: &[PathBuf],
    godot_path_flag: Option<&Path>,
    timeout: Duration,
) -> Result<Vec<(PathBuf, Issue)>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let godot_bin = find_godot_binary(godot_path_flag)?;
    let (groups, unrooted) = group_by_project(files);

    let mut out = Vec::new();
    for file in unrooted {
        out.push((
            file.clone(),
            Issue {
                code: "engine-project-not-found",
                severity: Severity::Error,
                line: 1,
                message: format!(
                    "no project.godot found in any ancestor of '{}'; cannot resolve a res:// path for engine verification",
                    file.display()
                ),
                fixable: false,
            },
        ));
    }

    for group in &groups {
        for (file, issue) in validate_group(&godot_bin, group, timeout)? {
            if let Some(issue) = issue {
                out.push((file, issue));
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize as TestCounter, Ordering as TestOrdering};

    static TMP_COUNTER: TestCounter = TestCounter::new(0);

    fn fresh_temp_dir(label: &str) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, TestOrdering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sg-engine-test-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // -- resolve_godot_binary: precedence and error messages ------------

    #[test]
    fn cli_flag_wins_when_it_points_to_a_real_file() {
        let dir = fresh_temp_dir("flag-real");
        let godot = dir.join("godot4.exe");
        fs::write(&godot, "").unwrap();
        let got = resolve_godot_binary(Some(&godot), Some("ignored"), Some(OsStr::new("ignored"))).unwrap();
        assert_eq!(got, godot);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cli_flag_pointing_nowhere_is_a_hard_error_not_a_fallback() {
        let dir = fresh_temp_dir("flag-missing");
        let missing = dir.join("does-not-exist.exe");
        let err = resolve_godot_binary(Some(&missing), None, None).unwrap_err();
        assert!(err.contains("--godot-path"), "{err}");
        assert!(err.contains("does not point to an executable file"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_var_used_when_no_flag_given() {
        let dir = fresh_temp_dir("env-real");
        let godot = dir.join("godot.exe");
        fs::write(&godot, "").unwrap();
        let got = resolve_godot_binary(None, Some(godot.to_str().unwrap()), None).unwrap();
        assert_eq!(got, godot);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_var_pointing_nowhere_is_a_hard_error() {
        let err = resolve_godot_binary(None, Some("Z:/definitely/not/a/real/path/godot.exe"), None).unwrap_err();
        assert!(err.contains("SG_GODOT"), "{err}");
    }

    #[test]
    #[cfg(windows)]
    fn path_search_finds_godot4_before_godot() {
        let dir = fresh_temp_dir("path-search");
        fs::write(dir.join("godot4.exe"), "").unwrap();
        fs::write(dir.join("godot.exe"), "").unwrap();
        let path_var = std::env::join_paths([&dir]).unwrap();
        let got = resolve_godot_binary(None, None, Some(&path_var)).unwrap();
        assert_eq!(got, dir.join("godot4.exe"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn path_search_falls_back_to_bare_godot() {
        let dir = fresh_temp_dir("path-fallback");
        fs::write(dir.join("godot.exe"), "").unwrap();
        let path_var = std::env::join_paths([&dir]).unwrap();
        let got = resolve_godot_binary(None, None, Some(&path_var)).unwrap();
        assert_eq!(got, dir.join("godot.exe"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_found_reports_the_full_search_order() {
        let dir = fresh_temp_dir("path-empty");
        let path_var = std::env::join_paths([&dir]).unwrap();
        let err = resolve_godot_binary(None, None, Some(&path_var)).unwrap_err();
        assert!(err.contains("--godot-path"), "{err}");
        assert!(err.contains("SG_GODOT"), "{err}");
        assert!(err.contains("godot4"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    // -- find_project_root ------------------------------------------------

    #[test]
    fn finds_project_root_several_levels_up() {
        let root = fresh_temp_dir("proj-root");
        fs::write(root.join("project.godot"), "").unwrap();
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("scene.tscn");
        fs::write(&file, "").unwrap();

        let found = find_project_root(&file).unwrap();
        assert_eq!(
            std::path::absolute(&found).unwrap(),
            std::path::absolute(&root).unwrap()
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn returns_none_when_no_project_godot_exists() {
        let dir = fresh_temp_dir("no-proj");
        let file = dir.join("scene.tscn");
        fs::write(&file, "").unwrap();
        // `dir` itself has no project.godot, and (barring an actual Godot
        // project somewhere above the OS temp dir, which would be
        // pathological) neither does anything above it.
        assert_eq!(find_project_root(&file), None);
        fs::remove_dir_all(&dir).ok();
    }

    // -- to_res_path --------------------------------------------------------

    #[test]
    fn computes_res_path_for_a_nested_file() {
        let root = PathBuf::from("/project");
        let file = PathBuf::from("/project/scenes/a/b.tscn");
        assert_eq!(to_res_path(&root, &file).as_deref(), Some("res://scenes/a/b.tscn"));
    }

    #[test]
    fn returns_none_for_a_file_outside_the_project_root() {
        let root = PathBuf::from("/project");
        let file = PathBuf::from("/elsewhere/b.tscn");
        assert_eq!(to_res_path(&root, &file), None);
    }

    // -- group_by_project -----------------------------------------------

    #[test]
    fn groups_files_by_project_and_separates_unrooted_ones() {
        let base = fresh_temp_dir("grouping");
        let proj_a = base.join("proj_a");
        let proj_b = base.join("proj_b");
        fs::create_dir_all(&proj_a).unwrap();
        fs::create_dir_all(&proj_b).unwrap();
        fs::write(proj_a.join("project.godot"), "").unwrap();
        fs::write(proj_b.join("project.godot"), "").unwrap();

        let a1 = proj_a.join("a1.tscn");
        let a2 = proj_a.join("nested").join("a2.tscn");
        let b1 = proj_b.join("b1.tscn");
        let orphan = base.join("orphan.tscn");
        fs::create_dir_all(proj_a.join("nested")).unwrap();
        for f in [&a1, &a2, &b1, &orphan] {
            fs::write(f, "").unwrap();
        }

        let (groups, unrooted) = group_by_project(&[a1.clone(), a2.clone(), b1.clone(), orphan.clone()]);
        assert_eq!(unrooted, vec![orphan]);
        assert_eq!(groups.len(), 2, "{groups:#?}");

        let group_a = groups.iter().find(|g| g.files.iter().any(|(f, _)| *f == a1)).unwrap();
        assert_eq!(group_a.files.len(), 2);
        assert_eq!(group_a.files[0].1, "res://a1.tscn");
        assert_eq!(group_a.files[1].1, "res://nested/a2.tscn");

        let group_b = groups.iter().find(|g| g.files.iter().any(|(f, _)| *f == b1)).unwrap();
        assert_eq!(group_b.files.len(), 1);

        fs::remove_dir_all(&base).ok();
    }

    // -- parse_validator_output ------------------------------------------

    #[test]
    fn parses_ok_and_fail_lines_and_ignores_noise() {
        let stdout = concat!(
            "Godot Engine v4.7.1.stable.official - https://godotengine.org\n",
            "\n",
            "SG-ENGINE-RESULT\tOK\tres://valid.tscn\t\n",
            "ERROR: some engine error unrelated to our marker\n",
            "SG-ENGINE-RESULT\tFAIL\tres://broken.tscn\tmissing dependency: res://x.gd\n",
            "SG-ENGINE-DONE\tFAIL\n",
        );
        let parsed = parse_validator_output(stdout);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get("res://valid.tscn"), Some(&LoadOutcome::Ok));
        assert_eq!(
            parsed.get("res://broken.tscn"),
            Some(&LoadOutcome::Fail("missing dependency: res://x.gd".to_string()))
        );
    }

    #[test]
    fn empty_or_garbage_output_parses_to_no_results() {
        assert!(parse_validator_output("").is_empty());
        assert!(parse_validator_output("not a marker line at all\nneither is this\n").is_empty());
    }

    // -- run_with_timeout: real child-process kill behavior --------------
    //
    // Windows: uses `cmd /C ping` as a portable, always-present way to
    // occupy a process for a controllable duration without adding a test
    // dependency. Unix: `sh -c sleep` plays the same role.

    #[cfg(windows)]
    fn slow_command(seconds: u32) -> Command {
        let mut c = Command::new("cmd");
        c.args(["/C", "ping", "-n", &(seconds + 1).to_string(), "127.0.0.1"]);
        c
    }
    #[cfg(windows)]
    fn fast_command() -> Command {
        let mut c = Command::new("cmd");
        c.args(["/C", "echo", "hello"]);
        c
    }

    #[cfg(unix)]
    fn slow_command(seconds: u32) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", &format!("sleep {seconds}")]);
        c
    }
    #[cfg(unix)]
    fn fast_command() -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", "echo hello"]);
        c
    }

    #[test]
    fn run_with_timeout_kills_a_hanging_process() {
        let start = Instant::now();
        let outcome = run_with_timeout(slow_command(10), Duration::from_millis(300)).unwrap();
        assert!(outcome.timed_out);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill did not actually cut the process short: took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn run_with_timeout_reports_normal_completion() {
        let outcome = run_with_timeout(fast_command(), Duration::from_secs(10)).unwrap();
        assert!(!outcome.timed_out);
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.stdout.contains("hello"), "{}", outcome.stdout);
    }

    // -- tail -------------------------------------------------------------

    #[test]
    fn tail_keeps_the_end_and_collapses_newlines() {
        // Last 3 characters of "a\nb\nc\nd\ne" are 'd', '\n', 'e'.
        let s = "a\nb\nc\nd\ne";
        assert_eq!(tail(s, 3), "...d e");
    }

    #[test]
    fn tail_returns_short_strings_unchanged_aside_from_trimming() {
        assert_eq!(tail("  hi  ", 100), "hi");
    }
}
