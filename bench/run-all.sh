#!/usr/bin/env bash
# Reproduces every measurement of docs/PERF.md. Run as root (drop_caches) from the repo root:
#   bench/run-all.sh [OUT_DIR]
# Prerequisites: pip install pyte python-xlib ; apt install xvfb xdotool x11-utils imagemagick strace
set -euo pipefail
OUT=${1:-bench/out}
mkdir -p "$OUT"
TUI=${TUI:-target/release/portico}
GUI=${GUI:-target/release/portico-gui}
if [[ ! -x $TUI ]]; then cargo build --release -p portico-tui; fi
if [[ ! -x $GUI ]]; then
  (cd apps/portico-gui && pnpm install --frozen-lockfile && pnpm build)
  cargo build --release -p portico-gui --features tauri/custom-protocol   # = tauri build --no-bundle
fi
P="python3 bench/perf.py"
run() { local name=$1; shift; echo "== $name" >&2; "$@" >"$OUT/$name.json" 2>"$OUT/$name.log"; }

# 1. TUI start (50 hosts + 2 bastions, 3 Include files, Match, ProxyJump)
run tui-start-cold  $P tui-start --bin "$TUI" --runs 20 --drop-caches
run tui-start-warm  $P tui-start --bin "$TUI" --runs 20 --warmup 2
# 2. TUI idle 30 s: host list, 1 and 5 sessions (terminal tab), syscalls during the window
run tui-idle-0      $P tui-idle --bin "$TUI" --seconds 30 --sessions 0
run tui-idle-1      $P tui-idle --bin "$TUI" --seconds 30 --sessions 1
run tui-idle-5      $P tui-idle --bin "$TUI" --seconds 30 --sessions 5
run tui-idle-0-strace $P tui-idle --bin "$TUI" --seconds 30 --sessions 0 --strace
run tui-idle-5-strace $P tui-idle --bin "$TUI" --seconds 30 --sessions 5 --strace
run tui-idle-5-alive15 $P tui-idle --bin "$TUI" --seconds 30 --sessions 5 --alive 15 --strace
run tui-idle-2000   $P tui-idle --bin "$TUI" --seconds 10 --hosts 2000 --includes 8
# 3. Big configs: first frame vs list, input during the load
for n in 200 500 1000; do run tui-load-$n $P tui-load --bin "$TUI" --hosts $n --includes 8 --runs 5; done
run tui-load-2000   $P tui-load --bin "$TUI" --hosts 2000 --includes 8 --runs 10 --probe 5
run tui-load-10000  $P tui-load --bin "$TUI" --hosts 10000 --includes 8 --runs 1 --seconds 180
# 4. GUI under Xvfb
run gui-start-warm  $P gui-start --bin "$GUI" --runs 10 --warmup 1
run gui-start-cold  $P gui-start --bin "$GUI" --runs 10 --drop-caches
run gui-start-nocomp $P gui-start --bin "$GUI" --runs 10 --warmup 1 --env WEBKIT_DISABLE_COMPOSITING_MODE=1
run gui-idle-0      $P gui-idle --bin "$GUI" --seconds 30 --sessions 0
run gui-idle-1      $P gui-idle --bin "$GUI" --seconds 30 --sessions 1
run gui-idle-5      $P gui-idle --bin "$GUI" --seconds 30 --sessions 5
run gui-idle-5-home $P gui-idle --bin "$GUI" --seconds 30 --sessions 5 --home-after
echo "results in $OUT" >&2
