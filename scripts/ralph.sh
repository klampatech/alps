#!/bin/bash
# Ralph Wiggum - Long-running AI agent loop
# Usage: ./ralph.sh [--tool amp|claude|codex] [max_iterations]
#
# On exit (any reason), writes .ralph-result.json with:
#   {iterations, elapsed_secs, completed: bool}
# so alps can report real metrics in receipts.

set -e

# Track start time so we can report elapsed_secs in the result file.
RALPH_START_EPOCH=$(date +%s)
RALPH_ITERATIONS=0
RALPH_COMPLETED=false
# RALPH_RESULT_FILE is set below after SCRIPT_DIR resolves.

# Parse arguments
TOOL="codex"  # ALPS default
MAX_ITERATIONS=10

while [[ $# -gt 0 ]]; do
  case $1 in
    --tool)
      TOOL="$2"
      shift 2
      ;;
    --tool=*)
      TOOL="${1#*=}"
      shift
      ;;
    *)
      # Assume it's max_iterations if it's a number
      if [[ "$1" =~ ^[0-9]+$ ]]; then
        MAX_ITERATIONS="$1"
      fi
      shift
      ;;
  esac
done

# Validate tool choice
if [[ "$TOOL" != "amp" && "$TOOL" != "claude" && "$TOOL" != "codex" ]]; then
  echo "Error: Invalid tool '$TOOL'. Must be 'amp', 'claude', or 'codex'."
  exit 1
fi
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# State files live in the ralph WORKING directory (which is $(pwd) when
# invoked by alps). The implement agent runs ralph.sh with cwd=workspace,
# so $(pwd) is the ralph workspace, not the SCRIPT_DIR.
# SCRIPT_DIR is only used for the prompt files (AGENTS.md / CLAUDE.md /
# prompt.md) which alps copies both into the workspace and lives at the
# source.
PRD_FILE="$(pwd)/prd.json"
PROGRESS_FILE="$(pwd)/progress.txt"
ARCHIVE_DIR="$(pwd)/archive"
LAST_BRANCH_FILE="$(pwd)/.last-branch"
# Codex writes its final assistant message here so the COMPLETE-signal grep
# doesn't false-positive on the prompt text (which mentions <promise>COMPLETE
# as instructions). Without this, the first iteration always matches the
# prompt echo and Ralph exits prematurely.
CODEX_LAST_MESSAGE="$(pwd)/.codex-last-message.txt"
RALPH_RESULT_FILE="$(pwd)/.ralph-result.json"

# Write the result file so alps can read it. Called from every exit path.
# Uses jq if available; falls back to printf if not.
write_ralph_result() {
  local now elapsed
  now=$(date +%s)
  elapsed=$((now - RALPH_START_EPOCH))
  if command -v jq >/dev/null 2>&1; then
    jq -n \
      --argjson iterations "$RALPH_ITERATIONS" \
      --argjson elapsed_secs "$elapsed" \
      --argjson completed "$RALPH_COMPLETED" \
      '{iterations: $iterations, elapsed_secs: $elapsed_secs, completed: $completed}' \
      > "$RALPH_RESULT_FILE"
  else
    printf '{"iterations": %s, "elapsed_secs": %s, "completed": %s}\n' \
      "$RALPH_ITERATIONS" "$elapsed" "$RALPH_COMPLETED" \
      > "$RALPH_RESULT_FILE"
  fi
}

# Archive previous run if branch changed
if [ -f "$PRD_FILE" ] && [ -f "$LAST_BRANCH_FILE" ]; then
  CURRENT_BRANCH=$(jq -r '.branchName // empty' "$PRD_FILE" 2>/dev/null || echo "")
  LAST_BRANCH=$(cat "$LAST_BRANCH_FILE" 2>/dev/null || echo "")
  
  if [ -n "$CURRENT_BRANCH" ] && [ -n "$LAST_BRANCH" ] && [ "$CURRENT_BRANCH" != "$LAST_BRANCH" ]; then
    # Archive the previous run
    DATE=$(date +%Y-%m-%d)
    # Strip "ralph/" prefix from branch name for folder
    FOLDER_NAME=$(echo "$LAST_BRANCH" | sed 's|^ralph/||')
    ARCHIVE_FOLDER="$ARCHIVE_DIR/$DATE-$FOLDER_NAME"
    
    echo "Archiving previous run: $LAST_BRANCH"
    mkdir -p "$ARCHIVE_FOLDER"
    [ -f "$PRD_FILE" ] && cp "$PRD_FILE" "$ARCHIVE_FOLDER/"
    [ -f "$PROGRESS_FILE" ] && cp "$PROGRESS_FILE" "$ARCHIVE_FOLDER/"
    echo "   Archived to: $ARCHIVE_FOLDER"
    
    # Reset progress file for new run
    echo "# Ralph Progress Log" > "$PROGRESS_FILE"
    echo "Started: $(date)" >> "$PROGRESS_FILE"
    echo "---" >> "$PROGRESS_FILE"
  fi
fi

# Track current branch
if [ -f "$PRD_FILE" ]; then
  CURRENT_BRANCH=$(jq -r '.branchName // empty' "$PRD_FILE" 2>/dev/null || echo "")
  if [ -n "$CURRENT_BRANCH" ]; then
    echo "$CURRENT_BRANCH" > "$LAST_BRANCH_FILE"
  fi
fi

# Initialize progress file if it doesn't exist
if [ ! -f "$PROGRESS_FILE" ]; then
  echo "# Ralph Progress Log" > "$PROGRESS_FILE"
  echo "Started: $(date)" >> "$PROGRESS_FILE"
  echo "---" >> "$PROGRESS_FILE"
fi

echo "Starting Ralph - Tool: $TOOL - Max iterations: $MAX_ITERATIONS"

for i in $(seq 1 $MAX_ITERATIONS); do
  RALPH_ITERATIONS=$i
  echo ""
  echo "==============================================================="
  echo "  Ralph Iteration $i of $MAX_ITERATIONS ($TOOL)"
  echo "==============================================================="

  # Run the selected tool with the ralph prompt
  if [[ "$TOOL" == "amp" ]]; then
    OUTPUT=$(cat "$SCRIPT_DIR/prompt.md" | amp --dangerously-allow-all 2>&1 | tee /dev/stderr) || true
  elif [[ "$TOOL" == "claude" ]]; then
    # Claude Code: use --dangerously-skip-permissions for autonomous operation, --print for output
    OUTPUT=$(claude --dangerously-skip-permissions --print < "$SCRIPT_DIR/CLAUDE.md" 2>&1 | tee /dev/stderr) || true
  else
    # Codex: --dangerously-bypass-approvals-and-sandbox = full autonomous mode.
    # The implement agent invokes ralph.sh with current_dir = the ralph workspace
    # (tasks/<id>/implementation/ralph/), so our process cwd IS the ralph dir.
    # -o writes the FINAL assistant message to a file, separate from the
    # streaming output. We use that file for the COMPLETE-signal grep below
    # to avoid false-positives on the prompt text (which itself contains
    # "<promise>COMPLETE</promise>" as instructions).
    RALPH_AGENTS="$(pwd)/AGENTS.md"
    if [[ ! -f "$RALPH_AGENTS" ]]; then
      RALPH_AGENTS="$SCRIPT_DIR/AGENTS.md"
    fi
    rm -f "$CODEX_LAST_MESSAGE"
    OUTPUT=$(codex exec --dangerously-bypass-approvals-and-sandbox -o "$CODEX_LAST_MESSAGE" < "$RALPH_AGENTS" 2>&1 | tee /dev/stderr) || true
  fi

  # Check for completion signal
  # For codex: grep the final-message file (avoids prompt-text false-positive).
  # For claude/amp: grep the streaming output (their --print mode doesn't echo the prompt).
  if [[ "$TOOL" == "codex" && -f "$CODEX_LAST_MESSAGE" ]]; then
    if grep -q "<promise>COMPLETE</promise>" "$CODEX_LAST_MESSAGE"; then
      echo ""
      echo "Ralph completed all tasks!"
      echo "Completed at iteration $i of $MAX_ITERATIONS"
      RALPH_COMPLETED=true
      write_ralph_result
      exit 0
    fi
  elif echo "$OUTPUT" | grep -q "<promise>COMPLETE</promise>"; then
    echo ""
    echo "Ralph completed all tasks!"
    echo "Completed at iteration $i of $MAX_ITERATIONS"
    RALPH_COMPLETED=true
    write_ralph_result
    exit 0
  fi

  echo "Iteration $i complete. Continuing..."
  sleep 2
done

echo ""
echo "Ralph reached max iterations ($MAX_ITERATIONS) without completing all tasks."
echo "Check $PROGRESS_FILE for status."
# RALPH_COMPLETED is still false; write_ralph_result reflects that.
write_ralph_result
exit 1
