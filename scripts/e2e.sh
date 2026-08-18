#!/usr/bin/env bash
#
# Turn-key dual-mode Playwright e2e runner for tmp-companion.
#
#   scripts/e2e.sh                  # OFFLINE (SimDevice) — fast, default, no hardware (~1.5 min)
#   scripts/e2e.sh offline copy     # OFFLINE, only the copy spec
#   scripts/e2e.sh online           # ONLINE (real device) — songs, copy, doctor, level in turn
#   scripts/e2e.sh online level     # ONLINE, only the level spec
#   scripts/e2e.sh online all       # ONLINE, the full set (= the default online set)
#   scripts/e2e.sh soak <N>         # ONLINE, attended: loop level-rerun.spec.ts N times —
#                                   # drift / engage-drop / stochastic device-state class
#
# OFFLINE is a near-passthrough to `playwright test` (Playwright starts/stops its own
# SimDevice e2e_server + vite). ONLINE is fully managed: it pre-flights the device via a
# handshake-verified server start, runs each heavy spec in its own invocation (the device is
# exclusive-seize and the level spec is two ~3-min tests), and ALWAYS recovers the unit on
# exit — reamp-off + guarded scratch-clear (400-409) + recall 001 — even on Ctrl-C or a
# failed/killed run (a killed level run otherwise strands the unit re-amp-engaged / input-muted).
#
# Both modes first kill any stale e2e_server across this worktree's whole port stride (filtered
# to e2e_server processes, so a sibling worktree's run is never touched), so neither can reuse a
# wrong-mode server (`reuseExistingServer: true` would otherwise make an "online" run hit
# SimDevice — a false-green pass — or vice-versa), and pre-build the binary so the cold compile
# is out of the timed path (it would otherwise blow the config's 180 s webServer timeout).
#
# Preconditions for ONLINE: the unit plugged in + RESTED, and Pro Control CLOSED (it holds the
# exclusive HID seize). A handshake failure is reported with that hint.
#
# Written to run under macOS system /bin/bash (3.2) — note the empty-array `set -u` guards.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

# Per-worktree port isolation: parallel e2e runs in sibling worktrees otherwise fight over
# the one bridge (:7600) + vite (:1421) port — each suite's stale-kill answering/killing the
# other's server (a nondeterministic false-fail class). Derive a stable per-worktree offset
# from the worktree path so each tree gets its own port pair; override with TMP_E2E_PORT /
# TMP_E2E_VITE_PORT. Exported so the Playwright configs, vite, and the Rust e2e_server all read
# the same values (they default to 7600/1421 when unset, preserving a bare `bunx playwright`).
# ponytail: cksum%200 — a collision between two of the handful of real worktrees merely shares
# ports (today's status quo); widen the modulus only if that ever bites.
#
# OFFLINE now claims a RANGE, not one port: each Playwright worker gets its own e2e_server on
# PORT+i. So the per-worktree offset is STRIDED — with a stride of 1 a neighbouring worktree's
# base would land inside this one's range and the two runs would kill each other's servers.
#
# The stride is a FIXED constant, deliberately NOT the live worker count: keying it to WORKERS
# would move a worktree's base port whenever the count changed, so a `TMP_E2E_WORKERS=1` run
# could not clean up after a 3-worker one and would strand orphaned servers. Fixed stride ⇒ a
# worktree's range is stable, and the stale-kill below always covers all of it.
# Vite stays one port per worktree (a single shared asset server).
#
# PORT_BASE is 7800, ABOVE the 7600-7799 window the previous single-port scheme could occupy
# (7600 + cksum%200). Two concurrent offline runs must never interfere, and during the window
# where some worktrees still run the OLD script that is only true if the two schemes cannot
# overlap at all: a strided range based at 7600 would have put THIS worktree at 7704-7711,
# squarely inside the legacy band, so a run here could kill a sibling's live server and vice
# versa. Disjoint bands make that structurally impossible rather than merely unlikely.
PORT_STRIDE=8
PORT_BASE=7800
WORKERS="${TMP_E2E_WORKERS:-3}"
# NB printf, not err() — the helpers are defined further down and this runs before them.
# The `case` rejects a non-numeric value FIRST: `[ x -gt y ]` on a non-integer errors out
# under `set -e` with bash's own message instead of this one.
case "$WORKERS" in
  ''|*[!0-9]*)
    printf '\033[31m✗ TMP_E2E_WORKERS=%s is not a positive integer\033[0m\n' "$WORKERS" >&2
    exit 2 ;;
esac
if [ "$WORKERS" -lt 1 ] || [ "$WORKERS" -gt "$PORT_STRIDE" ]; then
  printf '\033[31m✗ TMP_E2E_WORKERS=%s outside 1..%s (the per-worktree port stride) — raise PORT_STRIDE\033[0m\n' \
    "$WORKERS" "$PORT_STRIDE" >&2
  exit 2
fi
PORT_OFFSET=$(( $(printf '%s' "$REPO" | cksum | cut -d' ' -f1) % 200 ))
PORT="${TMP_E2E_PORT:-$((PORT_BASE + PORT_OFFSET * PORT_STRIDE))}"
VITE_PORT="${TMP_E2E_VITE_PORT:-$((1421 + PORT_OFFSET))}"
export TMP_E2E_PORT="$PORT" TMP_E2E_VITE_PORT="$VITE_PORT" TMP_E2E_WORKERS="$WORKERS"
# shellcheck source=scripts/device-lock.sh disable=SC1091
. "$REPO/scripts/device-lock.sh"

OFFLINE_CFG="e2e/playwright.config.ts"
ONLINE_CFG="e2e/playwright.online.config.ts"
MANIFEST="src-tauri/Cargo.toml"
LOG_DIR="${TMPDIR:-/tmp}/tmp-companion-e2e"
SERVER_LOG="$LOG_DIR/e2e_server.log"
# ── P5 external validation (ONLINE only) ──────────────────────────────────────────
# GATE SEMANTICS, stated identically here and in `.claude/rules/e2e.md`:
#   ffmpeg ABSENT   ⇒ advisory. A loud, VISIBLE skip line in this lane summary. Never a
#                     silent pass, but never red either — ffmpeg is not a build or CI
#                     dependency of this app.
#   ffmpeg PRESENT
#     + rows emitted ⇒ a REAL GATE. A target miss flips `fail` red (CLAUDE.md: never
#                      characterize or expected-fail a product bug to keep CI green).
#     + no rows      ⇒ nothing was leveled+re-measured this run; logged and passed over.
#
# The e2e_server PROCESS is what runs the strict re-measures (`e2e_measure_sound` →
# `leveller::measure_sound_asis_strict`), so these vars are exported into ITS
# environment: the same capture that produced the run's own number also writes the WAV
# and appends one identity-carrying line here (`src-tauri/src/validate_log.rs`). There is
# deliberately NO post-suite re-capture loop — re-driving `probe` afterwards would open
# fresh device sessions inside the 45–100 s lazy-save-commit window from a process whose
# save registry is empty (danger.md), reading PRE-save bytes and failing correct runs.
VALIDATE_LOG="$LOG_DIR/level-validate-expectations.jsonl"
VALIDATE_WAV_DIR="$LOG_DIR/level-validate-wavs"
# Budget cap. Each row costs one ffmpeg pass over a ~7 s WAV (fast) plus the disk the
# server already spent writing it — generous on purpose, but a POLICY rather than an
# accident: a runaway spec must announce that rows were dropped, not silently truncate.
VALIDATE_MAX_ROWS="${TMP_E2E_VALIDATE_MAX_ROWS:-40}"
# Row-count FLOOR for the strict lane. `level-strict.spec.ts` re-measures exactly nine
# sounds from the saved state — 1 base + 4 scenes (`for scene of [0,1,2,3]`) + 4
# footswitches (`SWITCH_JOBS`) — and each one is supposed to append a row here. Anything
# short means re-measures died before their capture, which emits NO row at all: the judge
# can only grade what it is handed, so a truncated log used to sail through as "fewer rows,
# all green". Keep in step with that spec if its sound set ever changes.
VALIDATE_STRICT_MIN_ROWS=9
# The validation tolerance the JUDGE will use, PINNED for this lane. `level-validate.sh`
# reads TMP_E2E_LEVEL_TOL_LU as its default tolerance, so an ambient export of, say, 50
# would make every row pass by arithmetic — a rubber stamp wearing a gate's clothes. The
# ceiling is generous (the documented default is 1.0 LU; the solver's own band is 0.3 plus
# recapture noise), so anything above it is a mistake, not a preference. CLAMP rather than
# refuse: aborting a 45-minute attended online run over an env var is the worse failure.
VALIDATE_TOL_CEILING=2.0
VALIDATE_TOL="${TMP_E2E_LEVEL_TOL_LU:-1.0}"
if ! awk -v t="$VALIDATE_TOL" 'BEGIN { exit !(t + 0 == t && t + 0 > 0) }' 2>/dev/null; then
  printf '\033[31m✗ TMP_E2E_LEVEL_TOL_LU=%s is not a positive number — using 1.0\033[0m\n' \
    "$VALIDATE_TOL" >&2
  VALIDATE_TOL=1.0
fi
if awk -v t="$VALIDATE_TOL" -v c="$VALIDATE_TOL_CEILING" 'BEGIN { exit !(t + 0 > c + 0) }'; then
  printf '\033[31m✗ TMP_E2E_LEVEL_TOL_LU=%s exceeds the %s LU ceiling this lane allows — CLAMPING to %s. A tolerance that wide cannot fail a real target miss.\033[0m\n' \
    "$VALIDATE_TOL" "$VALIDATE_TOL_CEILING" "$VALIDATE_TOL_CEILING" >&2
  VALIDATE_TOL="$VALIDATE_TOL_CEILING"
fi
export TMP_E2E_LEVEL_TOL_LU="$VALIDATE_TOL"
if command -v ffmpeg >/dev/null 2>&1; then HAVE_FFMPEG=1; else HAVE_FFMPEG=0; fi
mkdir -p "$LOG_DIR"

# ── parse args: a mode token (online|offline) + zero or more spec names (copy|level|songs|all) ──
MODE="offline"
SPECS=()
for a in "$@"; do
  case "$a" in
    online|offline|soak) MODE="$a" ;;
    -h|--help)
      cat >&2 <<'USAGE'
Usage: scripts/e2e.sh [online|offline] [copy|level|songs|doctor|all ...]
       scripts/e2e.sh soak <N>
  (no args)        OFFLINE — all specs vs SimDevice (fast, ~1.5 min, no hardware)
  offline copy     OFFLINE — only the copy spec
  online           ONLINE  — songs, copy, doctor, level vs the real unit (Pro Control closed)
  online level     ONLINE  — only the level spec
  soak <N>         ONLINE  — attended: loop level-rerun.spec.ts N times, print a per-run
                   ledger + end tally (drift / engage-drop / stochastic device-state class)
USAGE
      exit 0 ;;
    *) SPECS+=("$a") ;;
  esac
done

SERVER_PID=""

# ── helpers ───────────────────────────────────────────────────────────────────
log() { printf '\033[36m▸ %s\033[0m\n' "$*"; }
err() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; }

# A real build (cargo for e2e_server) panics in tauri-build if ./dist is absent (gitignored,
# missing in a fresh worktree). A stub index.html is enough — the live UI is served by vite.
ensure_dist() { [ -f dist/index.html ] || { mkdir -p dist; printf '<!doctype html><title>e2e</title>' > dist/index.html; }; }

# Build the e2e_server up front so the (potentially minutes-long, cold) compile is out of the
# timed server-start path; its exit code is the build check.
prebuild() {
  log "building e2e_server + probe (incremental)…"
  cargo build -q --manifest-path "$MANIFEST" --features e2e --bin e2e_server --bin probe \
    || { err "e2e_server/probe build failed"; exit 1; }
}

PROBE_BIN="src-tauri/target/debug/probe"

# Kill ONLY an `e2e_server` on a port — never a bystander.
#
# `lsof -ti tcp:N` is indiscriminate twice over: it lists any process with N on EITHER
# endpoint (so an outbound connection to a remote :N counts), and it cannot tell our server
# from someone else's. That was tolerable when the script killed ONE port; now it sweeps a
# whole stride, across a band that can reach ports other tools use. Filtering by process name
# keeps the sweep from ever reaching another worktree's run — or an unrelated dev server.
kill_port() {
  for pid in $(lsof -ti "tcp:$1" 2>/dev/null || true); do
    case "$(ps -o comm= -p "$pid" 2>/dev/null || true)" in
      *e2e_server) kill "$pid" 2>/dev/null || true ;;
    esac
  done
}

# Clear this worktree's whole bridge STRIDE (PORT .. PORT+PORT_STRIDE-1). Killing only the
# base port would leave workers 1..N-1 stale, and `reuseExistingServer` would silently adopt
# them — the false-green trap that makes an "online" run hit a leftover SimDevice, or vice
# versa. Sweeping the full stride rather than just $WORKERS also cleans up after a run that
# used MORE workers than this one.
kill_port_range() {
  i=0
  while [ "$i" -lt "$PORT_STRIDE" ]; do
    kill_port $(( PORT + i ))
    i=$(( i + 1 ))
  done
}

bridge_post() { # $1 = JSON body, $2 = timeout (s, default 60); echoes the response body
  curl -fsS -m "${2:-60}" -X POST "http://127.0.0.1:$PORT/invoke" \
    -H 'content-type: application/json' -d "$1" 2>/dev/null
}

# shellcheck disable=SC2329  # invoked transitively from the trap handler (cleanup → recover_device → post), which shellcheck doesn't statically trace
post() { # POST one /invoke command, best-effort (recovery must never fail the script)
  bridge_post "$1" "${2:-60}" >/dev/null 2>&1 || true
}

# shellcheck disable=SC2329  # invoked from the trap handler (cleanup), not statically traced
recover_device() {
  curl -fsS -m 5 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 || return 0  # server gone, nothing to do
  log "recovering device — reamp-off + stray sweep + recall 001"
  post '{"cmd":"e2e_reamp_off","args":{}}'
  # The scenario fixtures stay RESIDENT in the scratch slots between runs (the run-start
  # pristine-checking seed self-repairs anything a run leveled; clearing here only forced
  # the next run to re-import all 5 — ~2 min of device churn per run). Set
  # TMP_E2E_CLEAR_SCENARIO=1 for the on-demand net-zero clean.
  if [ "${TMP_E2E_CLEAR_SCENARIO:-}" = "1" ]; then
    # `e2e_clear_preset` is NAME-GUARDED and fail-closed (danger.md: a destructive op keyed
    # on a slot mapping must be confirmed against the slot's own contents). A stale name
    # therefore does not clobber anything — it just makes the clear a silent no-op, which
    # is worse than loud: the "net-zero" run leaves the fixtures resident. That is exactly
    # what happened here (this list still said `E2E Reference`/`Target 1`/`Target 2`/
    # `Realistic` after the P4-A rename, and never mentioned 405 at all).
    #
    # SINGLE SOURCE: derive both the slot and the name from the fixture file itself, so a
    # rename or a seventh fixture can never leave this list behind again. The parse is a
    # line-oriented grep over the two adjacent keys `scenario-presets.json` actually emits
    # (`"listIndex": N,` then `"name": "…",`) — jq is not a dependency of this repo.
    scenario_pairs="$(awk '
      /"listIndex"/ { if (match($0, /[0-9]+/)) idx = substr($0, RSTART, RLENGTH); next }
      /"name"/ { if (idx != "" && match($0, /"name"[ \t]*:[ \t]*"[^"]*"/)) {
          s = substr($0, RSTART, RLENGTH); sub(/^"name"[ \t]*:[ \t]*"/, "", s); sub(/"$/, "", s)
          print idx "\t" s; idx = "" } }
    ' "$REPO/e2e/fixtures/scenario-presets.json")"
    if [ -z "$scenario_pairs" ]; then
      err "TMP_E2E_CLEAR_SCENARIO=1 but no slot/name pairs parsed out of $REPO/e2e/fixtures/scenario-presets.json — refusing to guess; nothing cleared"
    else
      printf '%s\n' "$scenario_pairs" | while IFS="$(printf '\t')" read -r cslot cname; do
        [ -n "$cslot" ] || continue
        post "{\"cmd\":\"e2e_clear_preset\",\"args\":{\"slot\":$cslot,\"expectName\":\"$cname\"}}"
      done
    fi
  fi
  # Sweep stray scenario imports an aborted seed stranded elsewhere in the bank
  # (imports land at the first EMPTY slot anywhere; guarded, fail-closed). Long
  # timeout: N strays × clear can exceed the default 60 s cap.
  post '{"cmd":"e2e_clear_strays","args":{}}' 300
  post '{"cmd":"e2e_load_preset","args":{"slot":0}}'
  touch "${TMPDIR:-/tmp/}tmp-companion-device.lastop"  # feed the idle-aware pre-seed rest
}

# shellcheck disable=SC2329  # invoked via `trap cleanup EXIT INT TERM`, which shellcheck doesn't count as a use
cleanup() { # ONLINE only (offline execs Playwright, which owns teardown + has no device)
  local code=$?
  trap - EXIT INT TERM
  recover_device                          # guarded-clear is fail-closed: never touches a real preset
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  kill_port "$PORT"                       # cargo run spawns the binary as a child — kill by port too
  device_lock_release                     # release the machine-global device lock (online only)
  exit "$code"
}

# Wait until the managed ONLINE server prints its ready line AND answers /health, surfacing a
# device-handshake failure with an actionable hint. (Build failures are already caught by prebuild.)
wait_server_ready() {
  for _ in $(seq 1 240); do
    handshake_err=$(grep "device handshake failed" "$SERVER_LOG" 2>/dev/null | tail -1 || true)
    if [ -n "$handshake_err" ]; then
      err "$handshake_err"
      err "  → is the unit plugged in and Pro Control closed? (Pro Control holds the exclusive HID seize)"
      exit 1
    fi
    if grep -q "seeded snapshot from the real device" "$SERVER_LOG" 2>/dev/null \
       && curl -fsS -m 5 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  err "e2e_server not ready after 240s:"; tail -20 "$SERVER_LOG" >&2; exit 1
}

# ── always clear the stale bridge range first (the fake-mode-reuse guard), then warm the
#    binary. Offline claims PORT..PORT+WORKERS-1; online only uses PORT, and clearing the
#    rest of its range is harmless (nothing of its own is listening there). ──
kill_port_range
ensure_dist
prebuild

# ── OFFLINE: fast path — let Playwright own the (SimDevice) server + vite lifecycle ──
if [ "$MODE" = offline ]; then
  log "OFFLINE e2e (SimDevice) — Playwright manages servers"
  # Build the spec-file args in the positional params ("$@" is empty-safe under `set -u`,
  # unlike an empty array — avoids a two-arm exec).
  set --
  for s in "${SPECS[@]:-}"; do [ -n "$s" ] && [ "$s" != all ] && set -- "$@" "specs/$s.spec.ts"; done
  exec bunx playwright test --config "$OFFLINE_CFG" "$@"
fi

# ── ONLINE: managed — seed-first, handshake-verified start, per-spec runs, recovery ──
trap cleanup EXIT INT TERM

# Serialize the ONE device across sessions/worktrees before any device work (seed/handshake).
# Held elsewhere → wait (polls, honours a stale/dead owner); ~30 min ceiling then abort.
if ! device_lock_acquire "$REPO"; then
  err "could not acquire the device lock — another online/hw run is holding the unit"
  exit 1
fi

# Resolve the spec set: empty (→ "  ") OR `all` → the full ordered set (light → heavy).
# `doctor-apply.online` sits with the other doctor work, BEFORE any level* spec (the ordering
# guard below enforces that for `doctor`; this one writes and saves through the same paths).
# It was absent from this set AND self-skipping on an env var the Playwright process never
# sees, so it had never run in either tier despite existing to be the one-off HW validation.
case " ${SPECS[*]:-} " in *" all "*|"  ") SPECS=(songs copy doctor doctor-apply.online doctor-oracle.online level level-rerun level-strict) ;; esac

# ORDERING GUARD (enforced, not just documented): doctor must run BEFORE any leveling
# spec — every level* spec writes (the wizard always saves post-disclaimer), and
# leveling equalizes the relative scene loudness the doctor's consistency check keys
# on, so the reverse order silently weakens the doctor oracle. ONLY the bare `doctor`
# spec is order-sensitive: the other doctor specs (doctor-apply.online, doctor-oracle.online)
# work presets the level* specs never save to, and the run-start seed self-repairs, so
# they need no guard and the matcher stays exact on purpose.
seen_leveling=0
for s in "${SPECS[@]:-}"; do
  case "$s" in level|level-*) seen_leveling=1 ;; esac
  if [ "$s" = "doctor" ] && [ "$seen_leveling" -eq 1 ]; then
    err "spec order error: doctor must come BEFORE every level* spec in '${SPECS[*]:-}' — reorder the arguments"
    exit 2
  fi
done

# Seed the scenario presets from the RUNNER in a FRESH probe process per attempt —
# never from inside a spec: Playwright's per-test budget (300 s) can't absorb seed
# (~90–150 s) + retries, and the seed self-repairs (sweeps stray imports from any
# earlier aborted run) so retrying is pollution-safe.
#
# ORDER: the FIRST seed runs BEFORE the server starts, so its many fresh connections
# stay clear of the in-process open lockout (`0xe00002c5`) that aborted the original
# in-spec seeds mid-import (stranding stray copies in the user's bank). The server's
# own handshake then snapshots the already-seeded presets; later (inter-spec) seeds
# POST `e2e_mark_seeded` (a no-HID snapshot patch) so the specs' `ensureScenario`
# fallback finds the presets present.
seed_scenario() { # $1 = "pre" (no server yet — skip the snapshot patch) | "mid"
  "$PROBE_BIN" --seed-scenario >>"$LOG_DIR/seed.log" 2>&1 || return 1
  [ "$1" = pre ] || bridge_post '{"cmd":"e2e_mark_seeded","args":{}}' 30 | grep -q '"ok":true'
}

seed_with_retry() { # $1 = pre|mid; returns 0 once seeded, 1 after 4 failed attempts
  local attempt
  for attempt in 1 2 3 4; do
    log "seeding the scenario presets (attempt $attempt)…"
    if seed_scenario "$1"; then touch "${TMPDIR:-/tmp/}tmp-companion-device.lastop"; return 0; fi
    touch "${TMPDIR:-/tmp/}tmp-companion-device.lastop"
    if [ "$attempt" -lt 4 ]; then
      log "seed attempt $attempt failed — resting 120 s (open lockout) before retry"
      sleep 120
    fi
  done
  return 1
}

# Rest → seed (pre-server) → settle → start the handshake-verified e2e_server → patch in the
# seeded presets. Shared by the ordered online spec loop below AND `soak` — this exact
# device-open-rest-window + seed-race + fail-loud mark-seeded sequence must not drift between
# the two callers. Sets SERVER_PID; exits 1 (recoverable via the trap) on a seed/handshake failure.
start_online_server() {
  # Initial quiet rest, IDLE-AWARE: the lockout only threatens when the device was
  # touched recently (a run that just ended / an aborted seed) — an attended start
  # minutes later needs no rest at all. The stamp file records the last device op
  # (written by the recovery trap + after each seed); rest only the REMAINDER.
  local stamp="${TMPDIR:-/tmp/}tmp-companion-device.lastop" idle=999 rest=60
  if [ -f "$stamp" ]; then idle=$(( $(date +%s) - $(stat -f %m "$stamp" 2>/dev/null || echo 0) )); fi
  if [ "$idle" -lt "$rest" ]; then
    log "resting the unit before the first seed ($(( rest - idle )) s — device idle only ${idle}s)…"
    sleep $(( rest - idle ))
  else
    log "device idle ${idle}s ≥ ${rest}s — skipping the pre-seed rest"
  fi
  if ! seed_with_retry pre; then
    err "scenario seed failed after 4 attempts — aborting (nothing to recover: no server ran)"
    err "  → check nothing else holds the device (Pro Control, a stale server/app), rest a minute, rerun"
    exit 1
  fi

  # Settle before the server's handshake: the device's list read lags its own writes,
  # and the handshake list feeds the startup snapshot.
  sleep 10

  log "starting handshake-verified server on :$PORT"
  : > "$SERVER_LOG"
  # Truncate any PRIOR run's expectations + WAVs before this run's server can append —
  # a stale row would otherwise ride along into this run's validation pass. Only wired
  # when ffmpeg is present; UNSET (not merely empty) ⇒ emission stays a no-op and the
  # server does no extra work at all (`validate_log::log_path`).
  if [ "$HAVE_FFMPEG" -eq 1 ]; then
    : > "$VALIDATE_LOG"
    rm -rf "$VALIDATE_WAV_DIR"
    mkdir -p "$VALIDATE_WAV_DIR"
    export TMP_E2E_VALIDATE_LOG="$VALIDATE_LOG"
    export TMP_E2E_VALIDATE_WAV_DIR="$VALIDATE_WAV_DIR"
  else
    unset TMP_E2E_VALIDATE_LOG
    unset TMP_E2E_VALIDATE_WAV_DIR
  fi
  TMP_E2E_ONLINE=1 TMP_E2E_PORT="$PORT" \
    cargo run -q --manifest-path "$MANIFEST" --features e2e --bin e2e_server \
    >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!
  disown "$SERVER_PID" 2>/dev/null || true  # silence the shell's "Terminated" notice when cleanup kills it
  wait_server_ready
  # The presets are verifiably placed (the pre-server probe seed exited 0); patch the
  # snapshot in case the handshake's list read lagged the fresh writes. FAIL-LOUD:
  # a silently-failed patch would send the specs' ensureScenario fallback into the
  # lockout-prone in-process reseed this runner exists to avoid.
  if ! bridge_post '{"cmd":"e2e_mark_seeded","args":{}}' 30 | grep -q '"ok":true'; then
    err "failed to patch the seeded presets into the startup snapshot — aborting"
    exit 1
  fi
  log "device connected — snapshot includes the seeded presets"
}

# ── SOAK: attended online repetition of level-rerun.spec.ts (drift / engage-drop /
#    stochastic device-state class) — reuses the exact seed-first / handshake-verified /
#    always-recover machinery above; it just loops the ONE spec N times instead of the
#    ordered spec set below, with a per-run pass/fail/wall-time ledger + an end tally.
if [ "$MODE" = soak ]; then
  N="${SPECS[0]:-}"
  case "$N" in
    ''|*[!0-9]*|0)
      err "usage: scripts/e2e.sh soak <N>  (N = a positive run count)"
      exit 1 ;;
  esac
  log "SOAK: $N online run(s) of level-rerun.spec.ts (attended)"
  start_online_server

  pass=0; fail_seed=0; fail_spec=0
  log "resting the unit before run 1 (post-handshake settle)…"
  sleep 60

  run=1
  while [ "$run" -le "$N" ]; do
    if [ "$run" -gt 1 ]; then
      # Each run's own afterAll clears the scenario slots — reseed before every
      # run after the first (mirrors the online spec-loop's inter-spec reseed).
      log "resting the unit between runs…"
      sleep 60
      if ! seed_with_retry mid; then
        printf 'soak run %d/%s: FAIL (seed)  wall=0s\n' "$run" "$N"
        fail_seed=$((fail_seed + 1))
        run=$((run + 1))
        continue
      fi
    fi
    start=$(date +%s)
    run_log="$LOG_DIR/soak-run-$run.log"
    if bunx playwright test --config "$ONLINE_CFG" "specs/level-rerun.spec.ts" >"$run_log" 2>&1; then
      elapsed=$(( $(date +%s) - start ))
      printf 'soak run %d/%s: PASS  wall=%ss\n' "$run" "$N" "$elapsed"
      pass=$((pass + 1))
    else
      elapsed=$(( $(date +%s) - start ))
      printf 'soak run %d/%s: FAIL (spec)  wall=%ss  log=%s\n' "$run" "$N" "$elapsed" "$run_log"
      fail_spec=$((fail_spec + 1))
    fi
    run=$((run + 1))
  done

  fail_total=$((fail_seed + fail_spec))
  total=$((pass + fail_total))
  rate=0
  if [ "$total" -gt 0 ]; then rate=$(( pass * 100 / total )); fi
  printf '\nsoak tally: %d/%d passed (%d%%) — failures: %d seed, %d spec\n' \
    "$pass" "$total" "$rate" "$fail_seed" "$fail_spec"
  exit $(( fail_total > 0 ))
fi

log "ONLINE e2e (real device) — seeding the scenario presets before the server starts"
start_online_server

fail=0
first=1
for s in "${SPECS[@]}"; do
  if [ "$first" -eq 1 ]; then
    # Rest between the server-start handshake and the first spec's own device work
    # (the post-handshake line needs quiet before it serves reads reliably).
    log "resting the unit before the first spec (post-handshake settle)…"
    sleep 60
  else
    # ONE pristine-checking seed per run serves every spec: teardowns no longer clear
    # the scenario (fixtures stay RESIDENT; TMP_E2E_CLEAR_SCENARIO=1 for net-zero). A
    # spec that persists a STRUCTURAL edit over a fixture slot (copy, doctor-save)
    # clears the server's SCENARIO_VERIFIED flag on success, so the NEXT spec's
    # ensureScenario re-verifies the device and re-imports only what drifted — it does
    # not trust a fixture that's since been mutilated. Value-only drift (leveling
    # saves) is NOT in that set and is still handled by ORDERING: doctor must run
    # BEFORE level-strict — leveling equalizes the relative scene loudness the
    # doctor's consistency check keys on. The rest stays: spec teardown/startup
    # device ops sit right in the lockout's danger window.
    log "resting the unit between specs…"
    sleep 60
  fi
  first=0
  log "running specs/$s.spec.ts (online)"
  # No outer timeout: Playwright's own 300 s/test governs, except the two full-fixture
  # end-to-end tests (doctor.spec.ts, level.spec.ts) which override to 20/25 min via their
  # own test.setTimeout — so a hung spec fails forward in up to ~25 min, not 5. A short
  # wrapper here would kill any of them mid-run.
  if bunx playwright test --config "$ONLINE_CFG" "specs/$s.spec.ts"; then
    log "specs/$s.spec.ts PASSED"
  else
    err "specs/$s.spec.ts FAILED"; fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  log "all online specs passed"
  # ── P5 external validation ────────────────────────────────────────────────────
  # No re-capture here: the WAVs and expectation rows were already written BY THE
  # SERVER, at the moment each sound was strict-re-measured (see the TMP_E2E_VALIDATE_*
  # block at the top of this file for the gate semantics and for why re-driving `probe`
  # afterwards would read PRE-save bytes). All that is left is to judge them with a
  # meter this repo did not write.
  if [ "$HAVE_FFMPEG" -eq 0 ]; then
    log "############################################################"
    log "#  EXTERNAL VALIDATION SKIPPED — ffmpeg is not on PATH.     #"
    log "#  Nothing was independently measured. NOT a pass; install  #"
    log "#  ffmpeg to turn this lane into a real gate.               #"
    log "############################################################"
  else
    # NB a bare `[ -s f ] && rows=…` would ABORT under `set -e` on an empty log (the
    # compound list's non-zero status is not in a condition context) — hence the `if`.
    rows=0
    if [ -s "$VALIDATE_LOG" ]; then
      rows="$(grep -c . "$VALIDATE_LOG" 2>/dev/null || echo 0)"
    fi
    # ROW FLOOR — the false-green this branch used to be. "No rows emitted" was logged and
    # passed over unconditionally, which is only honest when nothing in the run was
    # SUPPOSED to emit any. When level-strict ran, nine rows were promised (see
    # VALIDATE_STRICT_MIN_ROWS at the top of this file); zero — or four — means captures
    # died before they could be recorded and the judge silently graded a subset. The floor
    # only applies when the strict spec actually ran, so `scripts/e2e.sh online songs`
    # keeps its legitimate skip, as does an ffmpeg-less box (handled above).
    ran_strict=0
    for s in "${SPECS[@]:-}"; do
      [ "$s" = "level-strict" ] && ran_strict=1
    done
    if [ "$ran_strict" -eq 1 ] && [ "$rows" -lt "$VALIDATE_STRICT_MIN_ROWS" ]; then
      err "external validation ROW FLOOR: level-strict ran but only $rows expectation row(s) were emitted, expected at least $VALIDATE_STRICT_MIN_ROWS (1 base + 4 scenes + 4 footswitches)"
      err "  → $((VALIDATE_STRICT_MIN_ROWS - rows)) sound(s) were never independently measured; a re-measure died before its capture. See $SERVER_LOG"
      fail=1
    fi
    if [ "$rows" -eq 0 ]; then
      log "external validation — no rows emitted this run (nothing was strict-re-measured); skipping the ffmpeg pass"
    else
      VALIDATE_FEED="$VALIDATE_LOG"
      if [ "$rows" -gt "$VALIDATE_MAX_ROWS" ]; then
        VALIDATE_FEED="$LOG_DIR/level-validate-expectations.capped.jsonl"
        head -n "$VALIDATE_MAX_ROWS" "$VALIDATE_LOG" > "$VALIDATE_FEED"
        err "external validation CAPPED at $VALIDATE_MAX_ROWS rows — $((rows - VALIDATE_MAX_ROWS)) rows DROPPED (raise TMP_E2E_VALIDATE_MAX_ROWS to check them all)"
      fi
      log "external validation — judging $rows recorded row(s) with ffmpeg ebur128 (tolerance ${TMP_E2E_LEVEL_TOL_LU} LU)…"
      set +e
      bash "$REPO/scripts/level-validate.sh" --expectations "$VALIDATE_FEED"
      vrc=$?
      set -e
      # Branch all three codes explicitly: a mid-run SKIP (3) must never be reported as a
      # target miss, and must never quietly pass either.
      case "$vrc" in
        0) log "external validation PASSED" ;;
        3) log "external validation SKIPPED (exit 3 — ffmpeg vanished mid-run; nothing was checked)" ;;
        *)
          err "external validation FAILED (exit $vrc) — a leveled sound missed its target under an independent ffmpeg read"
          fail=1
          ;;
      esac
    fi
  fi
else
  err "one or more online specs failed"
fi
exit "$fail"
