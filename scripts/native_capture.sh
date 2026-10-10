#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cache="${TMPDIR:-/tmp}/photocraft-native-capture"
mkdir -p "$cache"
binary="$cache/native_capture"
lock="$cache/recording.lock"
record_pid=''
release_lock() {
  # Only remove this invocation's lock. Never guess whether another lock is stale.
  owner=$(cat "$lock/owner.pid" 2>/dev/null || true)
  if [ "$owner" = "$$" ] || [ -z "$owner" ]; then
    rm -f "$lock/owner.pid" "$lock/recording.pid"
    rmdir "$lock" 2>/dev/null || true
  fi
}
stop_recording() {
  if [ -n "$record_pid" ]; then
    kill -TERM "$record_pid" 2>/dev/null || true
    wait "$record_pid" 2>/dev/null || true
    record_pid=''
  fi
  exit "$1"
}
case "${1:-}" in
  --*) ;;
  *)
    if ! mkdir "$lock" 2>/dev/null; then
      owner=$(cat "$lock/owner.pid" 2>/dev/null || true)
      echo "native-capture: another recording holds $lock (wrapper PID ${owner:-unknown}). Finish that capture first. A stale lock is never removed automatically." >&2
      exit 75
    fi
    trap release_lock 0
    trap 'stop_recording 129' HUP
    trap 'stop_recording 130' INT
    trap 'stop_recording 143' TERM
    printf '%s\n' "$$" > "$lock/owner.pid"
    ;;
esac
if [ ! -x "$binary" ] || [ "$root/scripts/native_capture.swift" -nt "$binary" ]; then
  build="$binary.$$"
  if ! xcrun swiftc -O -parse-as-library "$root/scripts/native_capture.swift" -o "$build"; then
    rm -f "$build"
    exit 1
  fi
  mv "$build" "$binary"
fi
case "${1:-}" in
  --*) exec "$binary" "$@" ;;
  *)
    caffeinate -u -t 10 >/dev/null 2>&1 &
    export PHOTOCRAFT_CAPTURE_KEEP_AWAKE=1
    "$binary" "$@" &
    record_pid=$!
    printf '%s\n' "$record_pid" > "$lock/recording.pid"
    # Watching the recorder PID ties sleep prevention to its actual lifetime.
    caffeinate -d -i -w "$record_pid" >/dev/null 2>&1 &
    if wait "$record_pid"; then result=0; else result=$?; fi
    record_pid=''
    exit "$result"
    ;;
esac
