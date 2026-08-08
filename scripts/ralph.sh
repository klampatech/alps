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

# Returns 0 (true) if every userStory in the prd has passes=true.
# Returns 1 (false) if any story is missing the passes field, has
# passes=false, or the prd is unreadable / has no userStories.
#
# This is the ralph-side equivalent of alps's §12 item 9
# ImplementError::IncompleteStories guard. The two together prevent
# codex from claiming "<promise>COMPLETE</promise>" while leaving
# stories unfinished.
all_stories_pass() {
  local prd="$1"
  if [[ ! -f "$prd" ]]; then
    return 1
  fi
  if ! command -v jq >/dev/null 2>&1; then
    # Without jq we can't be sure. Fail open (return 0) so we don't
    # accidentally block the orchestrator; alps's own guard will still
    # catch a phantom-completed run.
    return 0
  fi
  local total incomplete
  total=$(jq -r '.userStories | length' "$prd" 2>/dev/null || echo 0)
  if [[ "$total" == "0" ]]; then
    return 1
  fi
  incomplete=$(jq -r '[.userStories[] | select(.passes != true)] | length' "$prd" 2>/dev/null || echo "$total")
  [[ "$incomplete" == "0" ]]
}

# Echoes the count of stories still failing (passes != true) in the prd.
# Used for the "N stories still failing" diagnostic message.
remaining_stories() {
  local prd="$1"
  if [[ ! -f "$prd" ]] || ! command -v jq >/dev/null 2>&1; then
    echo "?"
    return
  fi
  jq -r '[.userStories[] | select(.passes != true)] | length' "$prd" 2>/dev/null || echo "?"
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
  #
  # IMPORTANT: all three tee invocations use `tee -a /dev/stderr` (append mode).
  # Without `-a`, tee opens the destination file with O_WRONLY|O_CREAT|O_TRUNC
  # by default, which TRUNCATES the file to zero on every invocation. When the
  # orchestrator's stderr (FD 2) is redirected to the same file (e.g. via a
  # smoke-test wrapper's `2> file`), the orchestrator's earlier `elog!` writes
  # (which use O_APPEND) get clobbered by tee's truncation. The orchestrator's
  # O_APPEND writes then go to the new "end of file" (byte 0 after truncate),
  # but `tee` is also writing from byte 0 with O_WRONLY, so the streams
  # interleave destructively.
  #
  # With `tee -a`, tee opens the file with O_APPEND. Both writers (orchestrator
  # + tee) atomically append to the end of the file, so neither clobbers the
  # other. This is what unblocks smoke-style stderr capture for the operator
  # pattern `exec alps run ... 2> file`.
  if [[ "$TOOL" == "amp" ]]; then
    OUTPUT=$(cat "$SCRIPT_DIR/prompt.md" | amp --dangerously-allow-all 2>&1 | tee -a /dev/stderr) || true
  elif [[ "$TOOL" == "claude" ]]; then
    # Claude Code: use --dangerously-skip-permissions for autonomous operation, --print for output
    OUTPUT=$(claude --dangerously-skip-permissions --print < "$SCRIPT_DIR/CLAUDE.md" 2>&1 | tee -a /dev/stderr) || true
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
    OUTPUT=$(codex exec --dangerously-bypass-approvals-and-sandbox -o "$CODEX_LAST_MESSAGE" < "$RALPH_AGENTS" 2>&1 | tee -a /dev/stderr) || true
  fi

  # Check for completion signal
  #
  # The original implementation (PR #6 era) just grepped for the literal
  # string "<promise>COMPLETE</promise>" in the codex final message. That
  # false-positives when codex writes prose denying completion — e.g.
  #
  #   "10 stories still incomplete (US-003 through US-012), so no
  #   `<promise>COMPLETE</promise>` is emitted."
  #
  # The literal string IS in the file (in a denial), grep -q matches, and
  # ralph.sh writes `completed: true` to .ralph-result.json. The alps
  # orchestrator's §12 item 9 guard (ImplementError::IncompleteStories)
  # catches this on the alps side, but the ralph result file already lies.
  #
  # The fix: after a positive grep, verify prd.json shows all stories
  # passing. This is the ralph-side equivalent of the alps §12 item 9
  # guard. If prd.json disagrees, treat the iteration as incomplete and
  # continue (don't exit 0, don't write completed=true).
  #
  # For non-codex tools (claude/amp), the same fix applies: cross-check
  # the prd before treating the grep as a real completion.
  #
  # Surfaced by smoke #8 (2026-08-06): 12-story run hit the bug at
  # iteration 2 of 20 — codex wrote a denial, grep matched, ralph.sh
  # claimed completed=true with 10/12 stories still failing.
  if [[ "$TOOL" == "codex" && -f "$CODEX_LAST_MESSAGE" ]]; then
    if grep -q "<promise>COMPLETE</promise>" "$CODEX_LAST_MESSAGE"; then
      # Verify prd.json: only treat as completed if all stories pass.
      if all_stories_pass "$PRD_FILE"; then
        echo ""
        echo "Ralph completed all tasks!"
        echo "Completed at iteration $i of $MAX_ITERATIONS"
        RALPH_COMPLETED=true
        write_ralph_result
        exit 0
      else
        # False positive: codex mentioned the string in prose but prd
        # disagrees. Continue iterating; the next codex invocation will
        # pick up the remaining stories.
        #
        # Avoid `local` here — this block is inside a for loop, not a
        # function, so `local` errors with "can only be used in a function".
        # The var name is unique enough not to collide.
        remaining=$(remaining_stories "$PRD_FILE")
        echo ""
        echo "codex mentioned <promise>COMPLETE> in prose but $remaining stories still failing in prd.json. Continuing iteration."
      fi
    fi
  elif echo "$OUTPUT" | grep -q "<promise>COMPLETE</promise>"; then
    if all_stories_pass "$PRD_FILE"; then
      echo ""
      echo "Ralph completed all tasks!"
      echo "Completed at iteration $i of $MAX_ITERATIONS"
      RALPH_COMPLETED=true
      write_ralph_result
      exit 0
    else
      remaining=$(remaining_stories "$PRD_FILE")
      echo ""
      echo "tool mentioned <promise>COMPLETE> in prose but $remaining stories still failing in prd.json. Continuing iteration."
    fi
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
