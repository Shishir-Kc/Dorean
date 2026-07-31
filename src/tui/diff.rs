//! Line-level diffs and unified-diff annotation for the TUI.
//!
//! [`diff_lines`] computes an LCS diff between two strings (used for
//! before/after pairs). [`annotate`] colorizes output that is already a
//! unified diff (`-`/`+`/`@@` lines), which is what tool results contain.

/// One line of a computed diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// `' '` unchanged, `'-'` removed, `'+'` added.
    pub marker: char,
    pub text: String,
}

/// The role of a line in a unified diff for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Context (unchanged) line.
    Same,
    /// A `-` removal.
    Removed,
    /// A `+` addition.
    Added,
    /// A hunk header (`@@ ... @@`) or file header (`---`, `+++`, `diff --git`).
    Hunk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotatedLine {
    pub kind: DiffKind,
    pub text: String,
}

/// Compute a line diff between two texts.
pub fn diff_lines(a: &str, b: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = a.lines().collect();
    let new: Vec<&str> = b.lines().collect();

    // Guard against quadratic blowups on huge inputs; degrade to "all removed,
    // then all added" which is still correct, just not minimal.
    if old.len().saturating_mul(new.len()) > 4_000_000 {
        let mut out = Vec::with_capacity(old.len() + new.len());
        for line in &old {
            out.push(DiffLine {
                marker: '-',
                text: (*line).to_string(),
            });
        }
        for line in &new {
            out.push(DiffLine {
                marker: '+',
                text: (*line).to_string(),
            });
        }
        return out;
    }

    let n = old.len();
    let m = new.len();
    // DP table of LCS lengths. u32 is plenty for capped sizes.
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    let width = m + 1;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if old[i] == new[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }

    // Walk back through the table to reconstruct the edit script.
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            out.push(DiffLine {
                marker: ' ',
                text: old[i].to_string(),
            });
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            out.push(DiffLine {
                marker: '-',
                text: old[i].to_string(),
            });
            i += 1;
        } else {
            out.push(DiffLine {
                marker: '+',
                text: new[j].to_string(),
            });
            j += 1;
        }
    }
    while i < n {
        out.push(DiffLine {
            marker: '-',
            text: old[i].to_string(),
        });
        i += 1;
    }
    while j < m {
        out.push(DiffLine {
            marker: '+',
            text: new[j].to_string(),
        });
        j += 1;
    }
    out
}

/// Whether `text` looks like a unified diff (from `git diff` or similar).
pub fn looks_like_unified_diff(text: &str) -> bool {
    let mut changed = 0usize;
    let mut hunks = 0usize;
    let mut total = 0usize;
    for line in text.lines() {
        total += 1;
        if line.starts_with("@@") {
            hunks += 1;
        }
        if (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            changed += 1;
        }
    }
    (hunks > 0 && changed > 0) || (changed * 10 >= total * 3)
}

/// Annotate a unified-diff block into colored lines.
pub fn annotate(text: &str) -> Vec<AnnotatedLine> {
    text.lines()
        .map(|line| {
            let kind = if line.starts_with("@@")
                || line.starts_with("diff --git")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
            {
                DiffKind::Hunk
            } else if line.starts_with('+') {
                DiffKind::Added
            } else if line.starts_with('-') {
                DiffKind::Removed
            } else {
                DiffKind::Same
            };
            AnnotatedLine {
                kind,
                text: line.to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_yield_context_only() {
        let lines = diff_lines("a\nb\nc\n", "a\nb\nc\n");
        assert!(lines.iter().all(|l| l.marker == ' '));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn detects_insertion_and_removal() {
        let lines = diff_lines("a\nb\n", "a\nx\nb\n");
        let markers: Vec<char> = lines.iter().map(|l| l.marker).collect();
        assert_eq!(markers, vec![' ', '+', ' ']);
        let lines = diff_lines("a\nb\n", "b\n");
        let markers: Vec<char> = lines.iter().map(|l| l.marker).collect();
        assert_eq!(markers, vec!['-', ' ']);
    }

    #[test]
    fn empty_inputs() {
        assert!(diff_lines("", "").is_empty());
        let lines = diff_lines("a", "");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].marker, '-');
    }

    #[test]
    fn annotates_unified_diff() {
        let text = "diff --git a/x b/x\n@@ -1,2 +1,2 @@\n old\n+new\n";
        let annotated = annotate(text);
        assert_eq!(annotated[0].kind, DiffKind::Hunk);
        assert_eq!(annotated[1].kind, DiffKind::Hunk);
        assert_eq!(annotated[2].kind, DiffKind::Same);
        assert_eq!(annotated[3].kind, DiffKind::Added);
    }

    #[test]
    fn detects_unified_diff_heuristic() {
        assert!(looks_like_unified_diff("@@ -1 +1 @@\n-old\n+new\n"));
        assert!(looks_like_unified_diff("-a\n+b\n-c\n+d\n"));
        assert!(!looks_like_unified_diff(
            "just a normal paragraph\nof text\n"
        ));
    }
}
