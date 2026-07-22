//! A small, dependency-free unified-diff renderer for `sg fix --dry-run`.
//!
//! Line-based, using a classic O(n*m) LCS. The files `sg` operates on are
//! individual `.tscn`/`.tres` scenes (at most a few thousand lines), so
//! the quadratic cost is not a practical concern, and avoiding an extra
//! dependency keeps the CLI's own footprint small.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn split_keepends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn diff_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Delete(i));
            i += 1;
        } else {
            ops.push(Op::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(j));
        j += 1;
    }
    ops
}

/// Group `ops` into hunks, each padded with up to `context` lines of
/// unchanged content on either side; hunks whose padding would overlap
/// (the gap between two change runs is small) are merged into one.
/// Returns `[start, end)` index ranges into `ops`.
fn build_hunks(ops: &[Op], context: usize) -> Vec<(usize, usize)> {
    let n = ops.len();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if matches!(ops[i], Op::Equal(..)) {
            i += 1;
            continue;
        }
        let mut j = i;
        let end = loop {
            while j < n && !matches!(ops[j], Op::Equal(..)) {
                j += 1;
            }
            let eq_start = j;
            while j < n && matches!(ops[j], Op::Equal(..)) {
                j += 1;
            }
            let eq_len = j - eq_start;
            if j >= n || eq_len > 2 * context {
                break eq_start + eq_len.min(context);
            }
            // Short equal run: the next change block (guaranteed to start
            // at `j` unless we hit EOF) merges into this same hunk.
        };
        let start = i.saturating_sub(context);
        let start = match groups.last() {
            Some(&(_, prev_end)) => start.max(prev_end),
            None => start,
        };
        groups.push((start, end));
        i = end;
    }
    groups
}

/// Render a unified diff between `old` and `new` under the given display
/// `path`. Returns an empty string if the two are identical.
pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a = split_keepends(old);
    let b = split_keepends(new);
    let ops = diff_ops(&a, &b);

    let mut old_at = vec![0usize; ops.len()];
    let mut new_at = vec![0usize; ops.len()];
    let (mut old_ptr, mut new_ptr) = (0usize, 0usize);
    for (k, op) in ops.iter().enumerate() {
        old_at[k] = old_ptr;
        new_at[k] = new_ptr;
        match op {
            Op::Equal(..) => {
                old_ptr += 1;
                new_ptr += 1;
            }
            Op::Delete(_) => old_ptr += 1,
            Op::Insert(_) => new_ptr += 1,
        }
    }

    let mut out = String::new();
    out.push_str(&format!("--- a/{path}\n"));
    out.push_str(&format!("+++ b/{path}\n"));

    let push_line = |out: &mut String, marker: char, text: &str| {
        out.push(marker);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    };

    for (start, end) in build_hunks(&ops, 3) {
        let old_start = old_at[start];
        let new_start = new_at[start];
        let old_count = ops[start..end].iter().filter(|o| !matches!(o, Op::Insert(_))).count();
        let new_count = ops[start..end].iter().filter(|o| !matches!(o, Op::Delete(_))).count();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start + 1,
            old_count,
            new_start + 1,
            new_count
        ));
        for op in &ops[start..end] {
            match *op {
                Op::Equal(oi, _) => push_line(&mut out, ' ', a[oi]),
                Op::Delete(oi) => push_line(&mut out, '-', a[oi]),
                Op::Insert(ni) => push_line(&mut out, '+', b[ni]),
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_input_produces_empty_diff() {
        assert_eq!(unified_diff("f.tscn", "a\nb\n", "a\nb\n"), "");
    }

    #[test]
    fn single_line_change_is_reported() {
        let d = unified_diff("f.tscn", "a\nb\nc\n", "a\nx\nc\n");
        assert!(d.contains("--- a/f.tscn"));
        assert!(d.contains("+++ b/f.tscn"));
        assert!(d.contains("@@"));
        assert!(d.contains("-b\n"));
        assert!(d.contains("+x\n"));
        assert!(d.contains(" a\n"));
        assert!(d.contains(" c\n"));
    }

    #[test]
    fn missing_trailing_newline_still_terminates_the_line() {
        let d = unified_diff("f.tscn", "a\nb", "a\nc");
        assert!(d.contains("-b\n"));
        assert!(d.contains("+c\n"));
    }

    #[test]
    fn distant_changes_produce_separate_hunks() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let new = "X\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\nY\n";
        let d = unified_diff("f.tscn", old, new);
        assert_eq!(d.matches("@@").count(), 4, "expected two separate hunks:\n{d}");
    }
}
