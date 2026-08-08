#!/bin/bash
# Test the {{DELIVERABLE_PATH}} placeholder substitution logic used in
# smoke wrappers (alps-tier4-smoke-wrapper.sh). The substitution is bash
# string-replace: ${PROMPT_TEMPLATE//\{\{DELIVERABLE_PATH\}\}/${DELIVERABLE_PATH}}.
#
# These tests pin the contract so a future refactor (e.g. moving to
# Python for richer templating) can't silently change the behavior.
#
# Usage:
#   bash tests/test_prompt_substitution.sh
#
# Exits 0 if all tests pass, 1 otherwise. Each test prints PASS/FAIL.

set -uo pipefail

PASSED=0
FAILED=0

assert_eq() {
    local label="$1"
    local expected="$2"
    local actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        echo "PASS: $label"
        PASSED=$((PASSED + 1))
    else
        echo "FAIL: $label"
        echo "  expected: |$expected|"
        echo "  actual:   |$actual|"
        FAILED=$((FAILED + 1))
    fi
}

assert_contains() {
    local label="$1"
    local needle="$2"
    local haystack="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        echo "PASS: $label"
        PASSED=$((PASSED + 1))
    else
        echo "FAIL: $label"
        echo "  expected to contain: |$needle|"
        echo "  actual:              |$haystack|"
        FAILED=$((FAILED + 1))
    fi
}

assert_not_contains() {
    local label="$1"
    local needle="$2"
    local haystack="$3"
    if [[ "$haystack" != *"$needle"* ]]; then
        echo "PASS: $label"
        PASSED=$((PASSED + 1))
    else
        echo "FAIL: $label"
        echo "  expected to NOT contain: |$needle|"
        echo "  actual:                  |$haystack|"
        FAILED=$((FAILED + 1))
    fi
}

# The substitution pattern. Kept verbatim from /tmp/alps-tier4-smoke-17-wrapper.sh
# line 127 so this test pins the actual production behavior.
substitute() {
    local template="$1"
    local path="$2"
    echo "${template//\{\{DELIVERABLE_PATH\}\}/${path}}"
}

# === TEST 1: substitute_single_placeholder ===
TEMPLATE1="Build a Vite app at {{DELIVERABLE_PATH}}."
RESULT1=$(substitute "$TEMPLATE1" "/tmp/alps-tier4-notes-18")
assert_eq "substitute_single_placeholder" \
    "Build a Vite app at /tmp/alps-tier4-notes-18." \
    "$RESULT1"

# === TEST 2: substitute_multiple_placeholders ===
TEMPLATE2="Workdir: {{DELIVERABLE_PATH}}-workdir
Deliverable: {{DELIVERABLE_PATH}}
Write everything inside {{DELIVERABLE_PATH}}.
Do NOT create files under /tmp/ or /home/ except {{DELIVERABLE_PATH}}-workdir."
RESULT2=$(substitute "$TEMPLATE2" "/tmp/alps-tier4-notes-18")
assert_eq "substitute_multiple_placeholders" \
    "Workdir: /tmp/alps-tier4-notes-18-workdir
Deliverable: /tmp/alps-tier4-notes-18
Write everything inside /tmp/alps-tier4-notes-18.
Do NOT create files under /tmp/ or /home/ except /tmp/alps-tier4-notes-18-workdir." \
    "$RESULT2"

# === TEST 3: no_placeholder_passes_through_unchanged ===
TEMPLATE3="This prompt has no placeholders. Just plain text.
Build at /tmp/something. Run npm test."
RESULT3=$(substitute "$TEMPLATE3" "/tmp/whatever")
assert_eq "no_placeholder_passes_through_unchanged" \
    "$TEMPLATE3" \
    "$RESULT3"

# === TEST 4: placeholder_with_path_containing_special_chars ===
# Bash string-replace is literal, not glob — so hyphens, underscores, dots,
# colons, and slashes are NOT wildcards. Only { and } have meaning inside
# ${var//pattern/replacement} via brace expansion. Verify hyphens don't
# break the substitution.
TEMPLATE4="Build at {{DELIVERABLE_PATH}}"
RESULT4=$(substitute "$TEMPLATE4" "/tmp/foo-bar_baz.2026-08-07")
assert_eq "placeholder_with_path_containing_special_chars" \
    "Build at /tmp/foo-bar_baz.2026-08-07" \
    "$RESULT4"

# === TEST 5: substitute all 11 placeholders in the real smoke-17 template ===
REAL_TEMPLATE=$(cat /tmp/alps-tier4-notes-prompt-17.txt)
RESULT5=$(substitute "$REAL_TEMPLATE" "/tmp/alps-tier4-notes-99")
# All 11 placeholders should be replaced — so the result must contain ZERO.
PLACEHOLDER_COUNT=$(echo "$REAL_TEMPLATE" | grep -c '{{DELIVERABLE_PATH}}')
# grep returns 0 matches with exit 1 — capture stdout AND don't pollute with
# the "|| echo" fallback when grep DID output a count. Use a tmp file to
# separate exit code from output.
TMPGREP=$(mktemp)
echo "$RESULT5" | grep -c '{{DELIVERABLE_PATH}}' > "$TMPGREP" 2>/dev/null || true
REMAINING=$(cat "$TMPGREP" | head -1)
rm -f "$TMPGREP"
assert_eq "smoke-17-template-original-has-11-placeholders" \
    "11" \
    "$PLACEHOLDER_COUNT"
assert_eq "smoke-17-template-all-placeholders-substituted" \
    "0" \
    "$REMAINING"
assert_contains "smoke-17-template-contains-substituted-path" \
    "/tmp/alps-tier4-notes-99" \
    "$RESULT5"

# === TEST 6: empty path substitutes to empty string (edge case) ===
RESULT6=$(substitute "Path: {{DELIVERABLE_PATH}}" "")
assert_eq "empty_path_substitutes_to_empty_string" \
    "Path: " \
    "$RESULT6"

echo ""
echo "============================="
echo "Passed: $PASSED"
echo "Failed: $FAILED"
echo "============================="

if [[ $FAILED -gt 0 ]]; then
    exit 1
fi
exit 0