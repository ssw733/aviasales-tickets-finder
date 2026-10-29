#!/usr/bin/env bash
set -u

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$APP_DIR/target/release/aviasales-pathfinder"
LOG="$APP_DIR/data/server.log"

cd "$APP_DIR" || exit 1
mkdir -p "$APP_DIR/data"

if pgrep -f "$BIN" >/dev/null 2>&1; then
  exit 0
fi

if [ ! -x "$BIN" ]; then
  echo "$(date -Iseconds) binary not found: $BIN" >> "$LOG"
  exit 1
fi

while true; do
  if [ -f "$LOG" ] && [ "$(stat -c%s "$LOG" 2>/dev/null || echo 0)" -gt 52428800 ]; then
    mv "$LOG" "$LOG.1"
  fi
  "$BIN" >> "$LOG" 2>&1
  echo "$(date -Iseconds) server exited, restarting in 5s" >> "$LOG"
  sleep 5
done
