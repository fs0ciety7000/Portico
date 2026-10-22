#!/usr/bin/env bash
# Desktop E2E test: the real Tauri binary under WebKitGTK, driven through
# WebDriver (tauri-driver + WebKitWebDriver) on a virtual X display, against
# a throwaway OpenSSH sshd. See docs/TESTING.md.
#
#   e2e/run.sh            build the debug app (embedded frontend), run the test
#   e2e/run.sh --no-build run the test against the existing binary
#   e2e/run.sh --serve    set everything up and wait (drive it by hand with
#                         the WebDriver session at http://127.0.0.1:4444)
#
# Environment: CARGO_TARGET_DIR (where the binary is), E2E_KEEP=1 keeps the
# throwaway directory (screenshots, logs) instead of removing it.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
gui=$(dirname "$here")
repo=$(cd "$gui/../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
app=$target/debug/portico-gui
mode=test
build=1
for arg in "$@"; do
  case $arg in
    --serve) mode=serve ;;
    --no-build) build=0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

for tool in sshd ssh-keygen ssh-agent Xvfb xdotool WebKitWebDriver tauri-driver python3 node; do
  if ! command -v "$tool" >/dev/null && [ ! -x "/usr/sbin/$tool" ]; then
    echo "missing: $tool (see docs/TESTING.md)" >&2
    exit 1
  fi
done

if [ "$build" = 1 ]; then
  # `tauri build --debug` embeds the built frontend (no dev server needed).
  (cd "$gui" && CARGO_TARGET_DIR=$target pnpm tauri build --debug --no-bundle)
fi
[ -x "$app" ] || { echo "no app binary at $app" >&2; exit 1; }

keep=${E2E_KEEP:-0}
if [ -n "${E2E_ROOT:-}" ]; then
  root=$E2E_ROOT
  keep=1 # given by the caller: never removed
else
  root=$(mktemp -d "${TMPDIR:-/tmp}/portico-e2e.XXXXXX")
fi
cleanup() {
  if [ -f "$root/pids" ]; then
    while read -r pid; do kill "$pid" 2>/dev/null || true; done <"$root/pids"
  fi
  if [ -f "$root/dirs" ]; then
    while read -r dir; do rm -rf "$dir"; done <"$root/dirs"
  fi
  if [ "$keep" = 1 ] || [ "$mode" = serve ]; then
    echo "kept: $root" >&2
  else
    rm -rf "$root"
  fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

"$here/env.sh" "$root"
# shellcheck source=/dev/null
. "$root/env"

display=:$((90 + RANDOM % 9))
Xvfb "$display" -screen 0 1280x860x24 -nolisten tcp >/dev/null 2>&1 &
echo $! >>"$root/pids"
export DISPLAY=$display
# No GPU here: software GL, and no compositing surprises in WebKitGTK.
export LIBGL_ALWAYS_SOFTWARE=1
export PORTICO_LOG=${PORTICO_LOG:-info}

# WebDriver launches any program a request names, and a web page can reach it
# with a CORS "simple" request: random ports by default, never the well-known 4444.
free_port() {
  python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}
driver_port=${E2E_DRIVER_PORT:-$(free_port)}
native_port=${E2E_NATIVE_PORT:-$(free_port)}
if [ "$mode" = serve ] && [ "$driver_port" = 4444 ] && [ "${E2E_I_KNOW:-}" != 1 ]; then
  echo "refusing --serve on the well-known port 4444 (set E2E_I_KNOW=1 to override)" >&2
  exit 2
fi
tauri-driver --port "$driver_port" --native-port "$native_port" >"$root/driver.log" 2>&1 &
echo $! >>"$root/pids"
for _ in $(seq 100); do
  python3 -c "import socket; socket.create_connection(('127.0.0.1', $driver_port), 0.2)" 2>/dev/null && break
  sleep 0.1
done

export E2E_APP=$app
export E2E_DRIVER=http://127.0.0.1:$driver_port
if [ "$mode" = serve ]; then
  echo "ready: E2E_ROOT=$root E2E_DRIVER=$E2E_DRIVER DISPLAY=$DISPLAY" >&2
  echo "warning: any local process or web page can drive this WebDriver and run a program as you;" >&2
  echo "         do not browse while it is up, never use it on a shared machine. Ctrl-C to stop." >&2
  while :; do sleep 3600 & wait $!; done
fi
node --test --test-concurrency=1 "$here/gui.e2e.ts"
