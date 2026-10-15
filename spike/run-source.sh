#!/usr/bin/env bash
# C00 source feed into CF Input A; restarts itself if CF drops the push. Usage: run-source.sh [rtmps|srt] [kbps]
. "$(dirname "$0")/env.sh"
TARGET=${1:-rtmps}; KBPS=${2:-3000}
redact_all "source-$TARGET.log"
secret_presence
while true; do
  "${AS[@]}" python3 "$SPIKE_DIR/source_feed.py" --target "$TARGET" --kbps "$KBPS"
  echo "SRCEXIT rc=$? t=$(date +%s.%N)"
  sleep 2
done
