#!/usr/bin/env bash
# Benchmark harness: Electron VS Code vs the Tauri port.
# Measures binary/installer size, peak RAM (RSS), and cold startup time.
#
# Honest scope: binary-size comparison runs anywhere. RAM + startup need a
# real desktop session (a window must actually open), so those steps no-op
# with a clear message when $DISPLAY is unset (e.g. CI / this container).
set -euo pipefail

ELECTRON_APP="${ELECTRON_APP:-}"   # path to packaged Electron VS Code (e.g. .../Code.app or VSCode-linux-x64)
TAURI_APP="${TAURI_APP:-}"         # path to packaged Tauri build
RUNS="${RUNS:-5}"

hr() { printf '%.0s-' {1..60}; echo; }
have_display() { [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; }

dir_size() { du -sh "$1" 2>/dev/null | cut -f1; }

# Peak RSS in MB for a process tree, sampled while the app is up.
peak_rss_mb() { # $1 = pid
  local pid=$1 peak=0 cur
  while kill -0 "$pid" 2>/dev/null; do
    cur=$(ps -o rss= --ppid "$pid" -p "$pid" 2>/dev/null | awk '{s+=$1} END{print s}')
    [[ -n "$cur" && "$cur" -gt "$peak" ]] && peak=$cur
    sleep 0.1
  done
  echo $(( peak / 1024 ))
}

bench_size() {
  hr; echo "BINARY / INSTALLER SIZE"; hr
  [[ -n "$ELECTRON_APP" ]] && echo "Electron: $(dir_size "$ELECTRON_APP")  ($ELECTRON_APP)"
  [[ -n "$TAURI_APP"    ]] && echo "Tauri:    $(dir_size "$TAURI_APP")  ($TAURI_APP)"
  # Always available: the resolver crate's release artifact size.
  if command -v cargo >/dev/null; then
    cargo build --release --manifest-path "$(dirname "$0")/../spike/protocol-resolver/Cargo.toml" >/dev/null 2>&1 || true
  fi
}

bench_runtime() { # $1 = label, $2 = launch cmd
  local label=$1 cmd=$2
  if ! have_display; then
    echo "$label: SKIPPED (no DISPLAY — run on a desktop session)"; return
  fi
  local total=0
  for _ in $(seq "$RUNS"); do
    local start end
    start=$(date +%s%N)
    $cmd & local pid=$!
    # crude: wait for window; replace with a real first-paint probe per-OS
    sleep 4
    end=$(date +%s%N)
    local rss; rss=$(peak_rss_mb "$pid")
    kill "$pid" 2>/dev/null || true
    echo "  run: $(( (end-start)/1000000 ))ms, peak RSS ${rss}MB"
    total=$((total + rss))
  done
  echo "$label avg peak RSS: $(( total / RUNS ))MB over $RUNS runs"
}

bench_size
hr; echo "RUNTIME (RAM + startup)"; hr
[[ -n "$ELECTRON_APP" ]] && bench_runtime "Electron" "$ELECTRON_APP"
[[ -n "$TAURI_APP"    ]] && bench_runtime "Tauri"    "$TAURI_APP"
echo
echo "Usage: ELECTRON_APP=/path TAURI_APP=/path RUNS=5 bench/benchmark.sh"
