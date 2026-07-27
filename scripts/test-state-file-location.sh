#!/bin/bash
# Test that ralph.sh writes state files to the workspace ($(pwd)),
# not to SCRIPT_DIR (the source script location).
#
# This guards against the bug fixed in the recent refactor: previously,
# ralph.sh used $SCRIPT_DIR for everything, which meant alps never found
# the .ralph-result.json it expected in the workspace.
set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKSPACE="$(mktemp -d /tmp/alps-ralph-location-test-XXXXXX)"

# Minimal stubs: ralph.sh only needs prd.json to exist (and a tool that
# immediately exits with a COMPLETE signal). We mock codex/claude/amp
# by creating a fake "codex" in PATH that emits the COMPLETE signal.

cat > "$WORKSPACE/prd.json" <<'EOF'
{
  "branchName": "alps/test",
  "userStories": [
    {"id": "US-001", "title": "x", "description": "x",
     "acceptanceCriteria": [], "priority": 1, "passes": true}
  ]
}
EOF

# Fake codex that just emits COMPLETE immediately and writes the -o file
# (real codex uses `-o <file>` to write its final message to a separate file).
FAKE_BIN="$(mktemp -d /tmp/alps-fake-bin-XXXXXX)"
cat > "$FAKE_BIN/codex" <<'EOF'
#!/bin/bash
# Mimic real codex: read stdin (the AGENTS.md prompt), and when invoked with
# -o <file>, write the COMPLETE signal to that file.
last_message_file=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) last_message_file="$2"; shift 2 ;;
    *) shift ;;
  esac
done
echo "<promise>COMPLETE</promise>"
if [[ -n "$last_message_file" ]]; then
  echo "<promise>COMPLETE</promise>" > "$last_message_file"
fi
exit 0
EOF
chmod +x "$FAKE_BIN/codex"

# Run ralph.sh from the workspace, with the fake codex in PATH.
cd "$WORKSPACE"
PATH="$FAKE_BIN:$PATH" "$REPO_ROOT/scripts/ralph.sh" --tool codex 5 >/dev/null 2>&1 || true

# Assertions: state files MUST be in the workspace, NOT in SCRIPT_DIR.
fail=0
for f in .ralph-result.json .codex-last-message.txt prd.json progress.txt; do
  if [[ ! -f "$WORKSPACE/$f" ]]; then
    echo "FAIL: $f missing from workspace ($WORKSPACE)"
    fail=1
  else
    echo "ok: $WORKSPACE/$f exists"
  fi
  if [[ -f "$REPO_ROOT/scripts/$f" && "$f" != "ralph.sh" ]]; then
    # SCRIPT_DIR is alps/scripts. None of these state files should be there.
    echo "FAIL: $f leaked into SCRIPT_DIR ($REPO_ROOT/scripts/$f)"
    fail=1
  fi
done

# Clean up
rm -rf "$WORKSPACE" "$FAKE_BIN"
rm -f "$REPO_ROOT/scripts/progress.txt" "$REPO_ROOT/scripts/.last-branch" \
      "$REPO_ROOT/scripts/.codex-last-message.txt" \
      "$REPO_ROOT/scripts/.ralph-result.json" \
      "$REPO_ROOT/scripts/prd.json"

exit $fail
