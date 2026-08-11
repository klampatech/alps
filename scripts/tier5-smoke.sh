#!/usr/bin/env bash
# Tier 5 smoke runner — in-repo launcher for /tmp/alps-tier5-notes.
#
# Why this exists (closes §12 P7 by architectural change):
# The Tier-4 canonical recipe used:
#   herdr wait output <pane> --match "^# ALPS — Done$" --timeout 3600000
# That 1h hard ceiling killed smoke #25 mid-implementation (Tier-4
# proof obligation §0 row "smoke #25"). Smoke #26 succeeded because the
# operator substituted a filesystem/telemetry monitor loop for `herdr
# wait` entirely. This script bakes that pattern into a reusable runner
# so future Tier-5+ smokes don't re-derive it and don't fall back to the
# brittle 1h `herdr wait` timeout.
#
# Pattern (smoke #26):
#   1. Create herdr workspace + pane
#   2. Launch the canonical Tier-N wrapper into the pane (exec alps in fg,
#      with strace + proctree + journalctl snapshots — same as Tier 4)
#   3. In THIS process, monitor filesystem mtimes + the wrapper's telemetry
#      log + the .alps-last-done sentinel. NO herdr wait.
#   4. Hard ceiling: 4h default (configurable via --budget-secs). On hit,
#      surface a structured "smoke timed out at budget" report and exit 1
#      without killing alps (alps keeps running; operator can decide).
#   5. On sentinel-touch (`# ALPS — Done` written to .alps-last-done via
#      receipts.json mtime advancing OR the wrapper's "smoke complete" meta
#      log line), surface a structured "smoke converged" report and exit 0.
#
# Usage:
#   ./scripts/tier5-smoke.sh \
#       --smoke-number 1 \
#       --workdir /tmp/alps-tier5-notes-workdir \
#       --deliverable-path /tmp/alps-tier5-notes \
#       --prompt-template /tmp/alps-tier5-notes-prompt.txt \
#       --log-prefix /tmp/alps-tier5-1-stderr \
#       [--budget-secs 14400]   # default 4h
#
# Output (to stdout, structured for easy grep):
#   [smoke-launch] workspace=wXX pane=wXX:p1 wrapper=/tmp/... wrapper-pid=NNN
#   [smoke-monitor] tick 1 elapsed=30s ralph_iter=?/20 phase=implement last_event=...
#   [smoke-monitor] tick 2 elapsed=60s ...
#   [smoke-converged] elapsed=NNNs verdict_dir=/tmp/...-preserved
#   OR
#   [smoke-budget-exceeded] elapsed=14400s budget=14400s last_event=...
#
# Exit codes:
#   0 — smoke converged (Judge ACCEPTED or reject-cycle converged naturally)
#   1 — smoke exceeded budget (alps may still be running)
#   2 — preflight failure (caller bug or environment not ready)
#
# NOT covered by this runner:
#   - Pattern B SIGPIPE on heavy stdout (see references/smoke-runner-truncation-diagnostic.md)
#     Mitigation: wrapper uses stdbuf -oL, no tee.
#   - alps code bugs (this runner can't fix bugs in alps itself; the
#     wrapper / canonical /tmp/alps-tier4-smoke-wrapper.sh is what handles
#     the orchestrator's diagnostic side).
#
# References:
#   - docs/tier5-spec.md — what Tier 5 actually verifies
#   - docs/tier4-spec.md — Tier 4 spec (the recipe this evolved from)
#   - /tmp/alps-tier4-smoke-wrapper.sh — canonical Tier-N wrapper
#     (the actual `alps run` invocation lives there, not here)

set -uo pipefail

# === ARGUMENT PARSING ===

SMOKE_NUMBER=""
WORKDIR=""
DELIVERABLE_PATH=""
PROMPT_TEMPLATE_FILE=""
LOG_PREFIX=""
BUDGET_SECS=14400  # 4h default — Tier 4 #25 was 3h+ ; #26 was 1h45m
MONITOR_INTERVAL_SECS=30

usage() {
  cat <<EOF
Usage: $0 --smoke-number N \\
          --workdir PATH \\
          --deliverable-path PATH \\
          --prompt-template PATH \\
          --log-prefix PATH \\
          [--budget-secs N]      (default: 14400 = 4h) \\
          [--monitor-interval N] (default: 30)

All five required flags must be passed. Flags are validated up-front;
exit 2 with a usage line on any error.
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --smoke-number)        SMOKE_NUMBER="$2"; shift 2 ;;
    --workdir)             WORKDIR="$2"; shift 2 ;;
    --deliverable-path)    DELIVERABLE_PATH="$2"; shift 2 ;;
    --prompt-template)     PROMPT_TEMPLATE_FILE="$2"; shift 2 ;;
    --log-prefix)          LOG_PREFIX="$2"; shift 2 ;;
    --budget-secs)         BUDGET_SECS="$2"; shift 2 ;;
    --monitor-interval)    MONITOR_INTERVAL_SECS="$2"; shift 2 ;;
    -h|--help)             usage ;;
    *)                     echo "[tier5-smoke] ERROR: unknown flag: $1" >&2; usage ;;
  esac
done

if [[ -z "${SMOKE_NUMBER}" || -z "${WORKDIR}" || -z "${DELIVERABLE_PATH}" || -z "${PROMPT_TEMPLATE_FILE}" || -z "${LOG_PREFIX}" ]]; then
  echo "[tier5-smoke] ERROR: --smoke-number, --workdir, --deliverable-path, --prompt-template, --log-prefix are all required" >&2
  usage
fi

if [[ ! -f "${PROMPT_TEMPLATE_FILE}" ]]; then
  echo "[tier5-smoke] ERROR: --prompt-template ${PROMPT_TEMPLATE_FILE} does not exist" >&2
  exit 2
fi

# === CANONICAL WRAPPER DISCOVERY ===
#
# The canonical Tier-N wrapper is /tmp/alps-tier4-smoke-wrapper.sh —
# this runner re-uses it for Tier 5 unchanged. The wrapper handles
# alps binary invocation, prompt file rendering, strace attachment,
# process-tree snapshots, and the receipts-preservation block at exit.
#
# If the canonical wrapper isn't present (fresh host, first Tier 5
# smoke), this runner fails preflight rather than re-implementing it.

CANONICAL_WRAPPER="/tmp/alps-tier4-smoke-wrapper.sh"
if [[ ! -x "${CANONICAL_WRAPPER}" ]]; then
  echo "[tier5-smoke] ERROR: canonical wrapper not found or not executable: ${CANONICAL_WRAPPER}" >&2
  echo "[tier5-smoke] hint: it's the Tier-4 wrapper, reused for Tier 5+; lives outside this repo by design" >&2
  exit 2
fi

# === ENV SETUP ===

export PATH="/home/kyle/Development/alps/target/debug:/home/kyle/.nvm/versions/node/v20.19.2/bin:/home/kyle/.local/bin:$PATH"
cd /home/kyle/Development/alps

# Wire up user-space strace install (same as canonical wrapper).
if [[ -f /tmp/alps-strace-bin/env.sh ]]; then
  # shellcheck disable=SC1091
  source /tmp/alps-strace-bin/env.sh
fi

# === HERDR WORKSPACE + PANE SETUP ===

WORKSPACE_JSON=$(mktemp -t "tier5-smoke-${SMOKE_NUMBER}-workspace.XXXXXX.json")
WORKSPACE_OUTPUT_LOG=$(mktemp -t "tier5-smoke-${SMOKE_NUMBER}-workspace.XXXXXX.log")

echo "[tier5-smoke] creating herdr workspace (cwd=/home/kyle/Development/alps, label=alps-tier5-smoke-${SMOKE_NUMBER})"
herdr workspace create \
    --cwd /home/kyle/Development/alps \
    --label "alps-tier5-smoke-${SMOKE_NUMBER}" \
    > "${WORKSPACE_JSON}" 2> "${WORKSPACE_OUTPUT_LOG}" \
    || { echo "[tier5-smoke] ERROR: herdr workspace create failed; see ${WORKSPACE_OUTPUT_LOG}" >&2; exit 2; }

WORKSPACE_ID=$(python3 -c "
import json, sys
with open('${WORKSPACE_JSON}') as f:
    d = json.load(f)
print(d['result']['workspace']['workspace_id'])
" 2>/dev/null) || { echo "[tier5-smoke] ERROR: could not parse workspace_id from ${WORKSPACE_JSON}" >&2; exit 2; }

PANE="${WORKSPACE_ID}:p1"
echo "[tier5-smoke] workspace=${WORKSPACE_ID} pane=${PANE}"

# === INVOKE CANONICAL WRAPPER INTO PANE ===

echo "[tier5-smoke] dispatching canonical wrapper into ${PANE}"
herdr pane send-text "${PANE}" \
    "${CANONICAL_WRAPPER} \
        --smoke-number ${SMOKE_NUMBER} \
        --workdir ${WORKDIR} \
        --deliverable-path ${DELIVERABLE_PATH} \
        --prompt-template ${PROMPT_TEMPLATE_FILE} \
        --log-prefix ${LOG_PREFIX}" \
    > "${WORKSPACE_OUTPUT_LOG}" 2>&1 \
    || { echo "[tier5-smoke] ERROR: send-text failed; see ${WORKSPACE_OUTPUT_LOG}" >&2; exit 2; }

herdr pane send-keys "${PANE}" Enter \
    > "${WORKSPACE_OUTPUT_LOG}" 2>&1 \
    || { echo "[tier5-smoke] ERROR: send-keys Enter failed; see ${WORKSPACE_OUTPUT_LOG}" >&2; exit 2; }

# Give the wrapper 8 seconds to install signal handlers + start alps.
# (Canonical wrapper has a 2s internal alps-alive check; we give it 4x margin.)
echo "[tier5-smoke] giving wrapper 8s to launch alps + install handlers"
sleep 8

# Confirm wrapper started by reading the meta log line
META_LOG="${LOG_PREFIX}-meta.log"
if [[ ! -f "${META_LOG}" ]]; then
  echo "[tier5-smoke] ERROR: wrapper did not write ${META_LOG} within 8s — preflight failed" >&2
  echo "[tier5-smoke] pane tail:" >&2
  herdr pane read "${PANE}" --source recent 2>&1 | tail -30 >&2
  exit 2
fi

WRAPPER_PID=$(grep -aE "^\\[smoke${SMOKE_NUMBER}-wrapper" "${META_LOG}" 2>/dev/null \
    | grep -aE "alps pid:" \
    | head -1 \
    | sed -E 's/.*alps pid: ([0-9]+).*/\1/')

if [[ -z "${WRAPPER_PID}" ]]; then
  echo "[tier5-smoke] WARNING: could not extract alps PID from ${META_LOG}; monitor will rely on filesystem only"
fi

TELEMETRY_LOG="${LOG_PREFIX}-telemetry.log"
STDERR_LOG="${LOG_PREFIX}.log"

echo "[tier5-smoke-launch] workspace=${WORKSPACE_ID} pane=${PANE} alps_pid=${WRAPPER_PID:-unknown} budget_secs=${BUDGET_SECS} monitor_interval_secs=${MONITOR_INTERVAL_SECS}"

# === MONITOR LOOP ===
#
# We watch THREE signals for convergence:
#   (a) .alps-last-done sentinel (Tier 4 sentinel — written at the end
#       of every successful outer-loop run; also written on natural
#       convergence in the Reject loop if it converges)
#   (b) receipts.json mtime advancing within the last 60s
#   (c) The wrapper's "smoke complete" meta-log line
#
# We surface state via stdout (greppable for the operator) and never
# call herdr wait. The pane itself may keep producing output after we
# exit — that's fine; the canonical wrapper does its own receipts
# preservation block when alps exits.

START_TIME_SECS=$(date +%s)
DEADLINE_SECS=$((START_TIME_SECS + BUDGET_SECS))
TICK=0
LAST_RECEIPTS_MTIME=0
LAST_REJECT_COUNT=0
LAST_ACCEPT_COUNT=0
LAST_EVENT="launch"

echo "[tier5-smoke-monitor] tick 0 elapsed=0s budget=${BUDGET_SECS}s last_event=${LAST_EVENT}"

while true; do
  TICK=$((TICK + 1))
  NOW=$(date +%s)
  ELAPSED=$((NOW - START_TIME_SECS))
  REMAINING=$((DEADLINE_SECS - NOW))

  if [[ ${REMAINING} -le 0 ]]; then
    echo "[tier5-smoke-budget-exceeded] elapsed=${ELAPSED}s budget=${BUDGET_SECS}s last_event=${LAST_EVENT} alps_pid=${WRAPPER_PID:-unknown}"
    echo "[tier5-smoke-budget-exceeded] NOTE: alps may still be running; this runner does NOT kill it. The wrapper's post-exit preservation block will fire when alps exits naturally."
    echo "[tier5-smoke-budget-exceeded] To check manually: tail -f ${STDERR_LOG} ; ls -la ${WORKDIR}/tasks/*/receipts.json"
    exit 1
  fi

  # --- Signal (a): .alps-last-done sentinel ---
  SENTINEL="${WORKDIR}/.alps-last-done"
  if [[ -f "${SENTINEL}" ]]; then
    SENTINEL_MTIME=$(stat -c %Y "${SENTINEL}" 2>/dev/null || echo 0)
    # Sentinel touched within last 5 minutes — likely the outer loop converged
    if [[ $((NOW - SENTINEL_MTIME)) -lt 300 ]]; then
      echo "[tier5-smoke-converged] elapsed=${ELAPSED}s sentinel_touched=${SENTINEL_MTIME} remaining_budget=${REMAINING}s last_event=sentinel"
      echo "[tier5-smoke-converged] preserve_dir=${LOG_PREFIX%-stderr}-preserved"
      echo "[tier5-smoke-converged] verdict: check ${LOG_PREFIX%-stderr}-preserved/receipts.json for canonical Judge verdict"
      exit 0
    fi
  fi

  # --- Signal (b): receipts.json mtime advancing ---
  LATEST_RECEIPT=$(find "${WORKDIR}/tasks" -name 'receipts.json' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -1 | awk '{print $2}')
  if [[ -n "${LATEST_RECEIPT}" && -f "${LATEST_RECEIPT}" ]]; then
    RECEIPT_MTIME=$(stat -c %Y "${LATEST_RECEIPT}" 2>/dev/null || echo 0)
    if [[ ${RECEIPT_MTIME} -gt ${LAST_RECEIPTS_MTIME} ]]; then
      LAST_RECEIPTS_MTIME=${RECEIPT_MTIME}
      LAST_EVENT="receipts.json touched (mtime=${RECEIPT_MTIME})"
      # Extract Judge verdict line if present
      VERDICT=$(python3 -c "
import json, sys
try:
    with open('${LATEST_RECEIPT}') as f:
        d = json.load(f)
    print(d.get('judge_model', 'unknown'), d.get('verdict', d.get('verdict_reason', 'unknown')))
except Exception as e:
    print('parse-error:', e)
" 2>/dev/null || echo "parse-error")
      LAST_EVENT="${LAST_EVENT} verdict=${VERDICT}"
    fi
  fi

  # --- Signal (c): wrapper meta-log "smoke complete" line ---
  if [[ -f "${META_LOG}" ]]; then
    if grep -aqE "^\\[smoke${SMOKE_NUMBER}-wrapper .*\\] smoke #${SMOKE_NUMBER} complete$" "${META_LOG}"; then
      echo "[tier5-smoke-converged] elapsed=${ELAPSED}s wrapper_meta_complete=true remaining_budget=${REMAINING}s last_event=wrapper-complete"
      echo "[tier5-smoke-converged] preserve_dir=${LOG_PREFIX%-stderr}-preserved"
      exit 0
    fi
  fi

  # --- Telemetry-derived progress signal ---
  RALPH_ITER=""
  PHASE="unknown"
  if [[ -f "${TELEMETRY_LOG}" ]]; then
    RALPH_ITER=$(grep -aE "ralph: iteration" "${TELEMETRY_LOG}" 2>/dev/null | tail -1 \
        | sed -E 's/.*iteration ([0-9]+)\/([0-9]+).*/\1\/\2/')
    if grep -aqE "\\[(plan|implement|review|judge):" "${TELEMETRY_LOG}" 2>/dev/null; then
      PHASE=$(grep -aE "\\[(plan|implement|review|judge):" "${TELEMETRY_LOG}" 2>/dev/null | tail -1 \
          | sed -E 's/.*\[(plan|implement|review|judge):.*/\1/')
    fi
    LAST_REJECT_COUNT=$(grep -ac "rejected" "${TELEMETRY_LOG}" 2>/dev/null || echo 0)
    LAST_ACCEPT_COUNT=$(grep -ac "accepted" "${TELEMETRY_LOG}" 2>/dev/null || echo 0)
  fi

  # --- Alive check: is alps still running? ---
  ALIVE="alive"
  if [[ -n "${WRAPPER_PID}" ]] && ! kill -0 "${WRAPPER_PID}" 2>/dev/null; then
    ALIVE="dead"
  fi

  echo "[tier5-smoke-monitor] tick ${TICK} elapsed=${ELAPSED}s remaining=${REMAINING}s phase=${PHASE} ralph_iter=${RALPH_ITER:-?} accepts=${LAST_ACCEPT_COUNT} rejects=${LAST_REJECT_COUNT} alps=${ALIVE} last_event=${LAST_EVENT}"

  sleep "${MONITOR_INTERVAL_SECS}"
done
