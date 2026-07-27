//! AGENTS.md — shared context file for ALPS agents.
//!
//! Each ALPS run gets a `tasks/<id>/AGENTS.md` that accumulates learnings from
//! each agent as the run progresses. The next agent in the loop reads the
//! accumulated file before doing its work.
//!
//! Source of truth:
//! - Implement (ralph) discovers patterns → writes to its own `progress.txt`
//!   inside the ralph workspace. We extract the `## Codebase Patterns` section
//!   and append it to the task-level AGENTS.md.
//! - Review and Judge read the AGENTS.md as part of their context.
//! - Plan on retry reads the AGENTS.md + the rejection feedback.
//!
//! File path: `<workdir>/tasks/<task-id>/AGENTS.md`

use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentsMdError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Read the AGENTS.md at `task_dir/AGENTS.md`. Returns an empty string if the
/// file doesn't exist yet (first run of a task).
pub fn read(task_dir: &Path) -> Result<String, AgentsMdError> {
    let path = task_dir.join("AGENTS.md");
    match fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(AgentsMdError::Io {
            path: path.display().to_string(),
            source: e,
        }),
    }
}

/// Overwrite AGENTS.md with `content`. Used for the initial write after
/// implement discovers patterns.
pub fn write(task_dir: &Path, content: &str) -> Result<(), AgentsMdError> {
    let path = task_dir.join("AGENTS.md");
    fs::write(&path, content).map_err(|e| AgentsMdError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

/// Append `content` to AGENTS.md. Creates the file if it doesn't exist.
/// Appends a blank line separator first so multiple appends don't fuse.
pub fn append(task_dir: &Path, content: &str) -> Result<(), AgentsMdError> {
    let path = task_dir.join("AGENTS.md");
    let existing = read(task_dir)?;
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(content);
    fs::write(&path, next).map_err(|e| AgentsMdError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

/// Extract the `## Codebase Patterns` section from ralph's `progress.txt`.
/// Returns an empty string if progress.txt doesn't exist or has no
/// Codebase Patterns section.
///
/// The section is delimited by `## Codebase Patterns` at the start of a line,
/// runs until the next `## ` heading or end of file, and is returned as-is
/// (including the leading `## Codebase Patterns` line).
pub fn extract_patterns(ralph_dir: &Path) -> Result<String, AgentsMdError> {
    let path = ralph_dir.join("progress.txt");
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => {
            return Err(AgentsMdError::Io {
                path: path.display().to_string(),
                source: e,
            })
        }
    };
    Ok(extract_section(&text, "## Codebase Patterns"))
}

/// Find a `## Heading` section in `text` and return it (including the
/// heading line). If not found, return empty string. The section runs until
/// the next `## ` heading or end of file.
fn extract_section(text: &str, heading: &str) -> String {
    let mut in_section = false;
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            if line.trim_start() == heading.trim_start() {
                in_section = true;
                out.push_str(line);
                out.push('\n');
            } else if in_section {
                // Hit the next heading — stop.
                break;
            }
        } else if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn unique_dir(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = PathBuf::from(format!("/tmp/alps-agents-md-test-{}-{}{}", label, pid, nanos));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_missing_file_returns_empty() {
        // First run: no AGENTS.md yet. read() should return "" not error.
        let dir = unique_dir("missing");
        let s = read(&dir).unwrap();
        assert_eq!(s, "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = unique_dir("write-read");
        write(&dir, "# patterns\n- foo\n").unwrap();
        assert_eq!(read(&dir).unwrap(), "# patterns\n- foo\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_to_existing_file_separates_with_blank_line() {
        // After implement, append Review's findings. Should not fuse with
        // the previous content.
        let dir = unique_dir("append");
        write(&dir, "## Codebase Patterns\n- foo\n").unwrap();
        append(&dir, "## Review findings\n- bar\n").unwrap();
        let s = read(&dir).unwrap();
        assert!(s.contains("## Codebase Patterns"));
        assert!(s.contains("- foo"));
        assert!(s.contains("## Review findings"));
        assert!(s.contains("- bar"));
        // Verify the separator: previous content should be followed by a blank
        // line before the new section.
        assert!(s.contains("- foo\n\n## Review"), "expected blank-line separator, got:\n{}", s);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_to_empty_file_does_not_add_leading_blank_line() {
        let dir = unique_dir("append-empty");
        // No prior write — append creates the file.
        append(&dir, "## First content\n").unwrap();
        let s = read(&dir).unwrap();
        assert!(s.starts_with("## First content"), "got: {}", s);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_patterns_picks_up_codebase_patterns_section() {
        // progress.txt has multiple sections; we want only ## Codebase Patterns.
        let dir = unique_dir("extract");
        fs::write(
            dir.join("progress.txt"),
            "# Ralph Progress Log\n\
             Started: 2026-07-27\n\
             ---\n\
             \n\
             ## Codebase Patterns\n\
             - use foo for bar\n\
             - never baz\n\
             \n\
             ## 2026-07-27 - US-001\n\
             - implemented foo\n\
             \n\
             ## 2026-07-27 - US-002\n\
             - implemented bar\n",
        )
        .unwrap();
        let s = extract_patterns(&dir).unwrap();
        assert!(s.contains("use foo for bar"));
        assert!(s.contains("never baz"));
        // Should NOT include the per-story entries.
        assert!(!s.contains("US-001"), "extract should stop at next heading, got: {}", s);
        assert!(!s.contains("implemented foo"), "got: {}", s);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_patterns_missing_progress_returns_empty() {
        // Backward compat: if ralph didn't write progress.txt (e.g. crash
        // before first story), extract returns "".
        let dir = unique_dir("no-progress");
        let s = extract_patterns(&dir).unwrap();
        assert_eq!(s, "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_patterns_no_section_returns_empty() {
        // progress.txt exists but has no ## Codebase Patterns section.
        let dir = unique_dir("no-section");
        fs::write(
            dir.join("progress.txt"),
            "# Ralph Progress Log\n## 2026-07-27 - US-001\n",
        )
        .unwrap();
        let s = extract_patterns(&dir).unwrap();
        assert_eq!(s, "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_section_handles_missing_heading() {
        let s = extract_section("hello world\n", "## Whatever");
        assert_eq!(s, "");
    }

    #[test]
    fn extract_section_captures_until_next_heading_or_eof() {
        let text = "\
## Target
- a
- b
## Other
- c
";
        let s = extract_section(text, "## Target");
        assert!(s.contains("## Target"));
        assert!(s.contains("- a"));
        assert!(s.contains("- b"));
        assert!(!s.contains("## Other"), "should stop before next heading");
        assert!(!s.contains("- c"), "should not include content after next heading");
    }
}
