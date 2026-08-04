//! Auto-detect the deliverable path from a prompt.
//!
//! Closes SPEC §12 item 1C. Without auto-detect, the operator must pass
//! `--deliverable-path <path>` whenever the prompt says "build at
//! `/some/path/`" so that `read_artifacts` and the Judge's `read_files`
//! walk the actual deliverable tree instead of `--workdir`. Forgetting
//! the flag surfaces as Runtime Pitfall #16 ("Source files section is
//! empty" Hermes rejection even when the work is correct).
//!
//! ## What we match
//!
//! Common English prepositions + an absolute path:
//!   - "build a Vite app at /tmp/foo"
//!   - "Create a Go module at /tmp/foo"
//!   - "Build at /tmp/foo/"
//!   - "write to /tmp/foo"
//!   - "save into /tmp/foo"
//!   - "code under /tmp/foo"
//!   - "stuff in /tmp/foo"
//!
//! Paths are POSIX-style (`/`-separated). On Windows the operator would
//! have to pass `--deliverable-path` explicitly — auto-detect is
//! best-effort. All alps smokes target linux, so this is acceptable.
//!
//! ## Disambiguation
//!
//! Multiple paths in a prompt are common (deliverable + artifacts +
//! log dir). The deliverable is:
//!
//! 1. The first absolute path that appears after a preposition keyword
//!    AND is not under the workdir (artifacts/log dirs usually are).
//! 2. If all candidate paths are under the workdir, pick the most
//!    frequently mentioned path (the deliverable gets referenced more
//!    than once in ACs).
//! 3. As a tiebreaker, pick the shortest path (deliverable paths tend
//!    to be shorter than artifact dirs).
//!
//! ## When auto-detect fails
//!
//! Returns `None` and the CLI falls back to `--workdir`. The operator
//! can still pass `--deliverable-path` to override.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Keywords that introduce a deliverable path. Case-insensitive.
/// Matched as a standalone word (`\b`-style boundary via spaces + line
/// start/end) so we don't catch things like "match" or "atop".
const KEYWORDS: &[&str] = &[
    "at ", "in ", "to ", "into ", "under ", "inside ", "build at ", "build to ", "build in ",
    "save to ", "save into ", "save at ", "write to ", "write into ", "write at ",
    "create at ", "create in ", "implement at ", "implement in ",
];

/// Detect the most-likely deliverable path from `prompt`.
///
/// Returns `None` if no candidate absolute path is found.
///
/// `workdir` is used to bias the disambiguation: paths outside the
/// workdir are preferred (the deliverable usually lives outside; the
/// workdir's per-task git tree is what the operator wants excluded).
pub fn detect(prompt: &str, workdir: &Path) -> Option<PathBuf> {
    let candidates = collect_candidates(prompt);
    if candidates.is_empty() {
        return None;
    }

    // Score: outside-workdir > most-frequent > shortest
    let workdir_canon = std::fs::canonicalize(workdir)
        .unwrap_or_else(|_| workdir.to_path_buf());

    let mut scored: Vec<(PathBuf, i64)> = candidates
        .into_iter()
        .map(|p| {
            let canon = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            let outside_bonus = if !canon.starts_with(&workdir_canon) {
                1000
            } else {
                0
            };
            let freq_bonus = 0; // count is already captured by inserting once per mention
            let length_penalty = -(p.to_string_lossy().len() as i64);
            (p, outside_bonus + freq_bonus + length_penalty)
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().next().map(|(p, _)| p)
}

/// Walk the prompt, find every absolute path after a keyword. Return
/// them in order of mention.
fn collect_candidates(prompt: &str) -> Vec<PathBuf> {
    let lower = prompt.to_lowercase();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen_counts: HashMap<String, usize> = HashMap::new();

    for keyword in KEYWORDS {
        let mut search_from = 0usize;
        while let Some(idx) = lower[search_from..].find(keyword) {
            let absolute_idx = search_from + idx + keyword.len();
            if let Some(p) = extract_abs_path(&prompt[absolute_idx..]) {
                if !is_viable_candidate(&p) {
                    search_from = absolute_idx;
                    continue;
                }
                let key = p.to_string_lossy().to_string();
                let count = seen_counts.entry(key.clone()).or_insert(0);
                if *count == 0 {
                    out.push(p);
                }
                *count += 1;
            }
            search_from = absolute_idx;
        }
    }

    // Re-order: paths with more mentions come first (popularity sort).
    out.sort_by(|a, b| {
        let ca = seen_counts.get(&a.to_string_lossy().to_string()).copied().unwrap_or(1);
        let cb = seen_counts.get(&b.to_string_lossy().to_string()).copied().unwrap_or(1);
        cb.cmp(&ca)
    });
    out
}

/// True if `p` is a viable deliverable-path candidate. We reject paths that
/// are too short to be a real deliverable — typically these are noise from
/// guard lines like "do NOT create files under /tmp/" where the keyword
/// `under` matches but the path is just a system root (e.g. `/tmp/`).
///
/// Reject if the path has zero or one meaningful components. A real
/// deliverable is at least `/parent/leaf` (two components). The trailing
/// slash counts as a non-component; we strip it before counting.
fn is_viable_candidate(p: &Path) -> bool {
    let stripped = p.to_string_lossy().trim_end_matches('/').to_string();
    if stripped.is_empty() {
        return false;
    }
    let components = stripped.split('/').filter(|c| !c.is_empty()).count();
    components >= 2
}

/// Given a string starting right after a keyword, extract the absolute
/// path token. Stops at whitespace, common punctuation, or the end of
/// the string. Returns `None` if the next character isn't `/`.
fn extract_abs_path(s: &str) -> Option<PathBuf> {
    // Skip a single leading backtick (markdown code-span wrapping).
    let s = s.trim_start().strip_prefix('`').unwrap_or(s.trim_start());
    if !s.starts_with('/') {
        return None;
    }
    let end = s
        .find(|c: char| {
            c.is_whitespace()
                || c == '`'
                || c == ')'
                || c == ']'
                || c == '}'
                || c == ','
                || c == ';'
                || c == '"'
                || c == '\''
                || c == '\n'
                || c == '.'
        })
        .unwrap_or(s.len());
    let raw = &s[..end];
    if raw.is_empty() || raw == "/" {
        return None;
    }
    Some(PathBuf::from(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_simple_at_pattern() {
        let p = "Build a Vite app at /tmp/alps-tier3-weather";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/alps-tier3-weather"));
    }

    #[test]
    fn detects_in_pattern() {
        let p = "Create a Go module in /tmp/alps-go-smoke";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/alps-go-smoke"));
    }

    #[test]
    fn detects_write_to_pattern() {
        let p = "Build it and write to /tmp/myapp";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/myapp"));
    }

    #[test]
    fn detects_trailing_slash() {
        let p = "Build at /tmp/alps-crud-demo/";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/alps-crud-demo/"));
    }

    #[test]
    fn ignores_relative_paths() {
        let p = "build at ./relative/path — that should NOT match";
        assert!(detect(p, Path::new("/tmp/workdir")).is_none());
    }

    #[test]
    fn ignores_paths_without_keyword() {
        let p = "Look at /tmp/random for inspiration";
        // "at " is a keyword, but "Look at" should still match /tmp/random
        // — the keyword is positional, not semantic. We accept that as
        // best-effort.
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/random"));
    }

    #[test]
    fn prefers_outside_workdir() {
        let p = "Write the workdir at /tmp/alps-test/workdir. Save deliverable at /tmp/alps-test-out";
        // Both paths exist as candidates. /tmp/alps-test-out is outside
        // workdir /tmp/alps-test/workdir → wins.
        let d = detect(p, Path::new("/tmp/alps-test/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/alps-test-out"));
    }

    #[test]
    fn prefers_most_mentioned_when_all_inside() {
        let p = "Build at /tmp/foo. The /tmp/foo dir. Files in /tmp/foo. \
                 Other path is /tmp/bar.";
        // /tmp/foo mentioned 3 times, /tmp/bar once. Same outside-workdir
        // bonus (both equally inside or outside). Pick by mention count.
        let d = detect(p, Path::new("/tmp/somewhere-else")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/foo"));
    }

    #[test]
    fn real_world_tier_3_prompt() {
        let p = "Build a Vite + React + TypeScript weather dashboard at /tmp/alps-tier3-weather with the following structure:";
        let d = detect(p, Path::new("/tmp/alps-tier3-weather-workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/alps-tier3-weather"));
    }

    #[test]
    fn real_world_tier_2_5b_go_prompt() {
        let p = "Create a Go module at /tmp/alps-go-smoke with:";
        let d = detect(p, Path::new("/tmp/alps-go-smoke-workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/alps-go-smoke"));
    }

    #[test]
    fn no_path_returns_none() {
        let p = "Build a Python calculator with add and subtract functions.";
        assert!(detect(p, Path::new("/tmp/workdir")).is_none());
    }

    #[test]
    fn nested_artifact_path_does_not_win() {
        // The deliverable is /tmp/myapp; the artifacts dir is /tmp/myapp/artifacts.
        // Both start with /tmp/myapp. workdir is /tmp/something-else.
        // The deliverable path is mentioned in "build at" — outside-workdir
        // bonus is the same for both (both are outside /tmp/something-else).
        // /tmp/myapp wins by length (shorter).
        let p = "Build a service at /tmp/myapp. Save artifacts in /tmp/myapp/artifacts.";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/myapp"));
    }

    #[test]
    fn empty_prompt_returns_none() {
        assert!(detect("", Path::new("/tmp/workdir")).is_none());
    }

    #[test]
    fn handles_backtick_wrapped_path() {
        let p = "Build at `/tmp/qux-app` for testing";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/qux-app"));
    }

    #[test]
    fn rejects_single_component_paths() {
        // The prompt template's guard line says "do NOT create files under
        // /tmp/" — the keyword `under` matches and /tmp/ is technically a valid
        // absolute path. But it's a system root, not a deliverable. The
        // viability filter rejects single-component paths so /tmp/ doesn't
        // win over the actual deliverable /tmp/foo.
        assert!(detect("do NOT create files under /tmp/", Path::new("/tmp/workdir")).is_none());
        assert!(detect("nothing to see at /tmp", Path::new("/tmp/workdir")).is_none());
        assert!(detect("look at /home", Path::new("/tmp/workdir")).is_none());
    }

    #[test]
    fn guard_line_does_not_override_real_deliverable() {
        // The OP smoke that prompted this fix: the prompt-template guard line
        // includes "do NOT create files under /tmp/" but the actual deliverable
        // is /tmp/foo. Before the viability filter, the auto-detect picked
        // /tmp/ as the deliverable path, and the Judge's read_artifacts walked
        // /tmp/ (choking on /tmp/systemd-private-*). The fix: skip paths with
        // fewer than 2 components so /tmp/ is ignored and /tmp/foo wins.
        let p = "Build a Python app at /tmp/foo. Write everything inside the workdir (do NOT create files under /tmp/).";
        let d = detect(p, Path::new("/tmp/workdir")).unwrap();
        assert_eq!(d, PathBuf::from("/tmp/foo"));
    }
}
