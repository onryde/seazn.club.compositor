#!/usr/bin/env bash
# C00 one SRT playback reader. Usage: run-reader.sh <tag> <seconds> [extra srt_reader.py args]
. "$(dirname "$0")/env.sh"
TAG=$1; SECS=$2; shift 2
redact_all "reader-$TAG.log"
"${AS[@]}" python3 "$SPIKE_DIR/srt_reader.py" --tag "$TAG" --seconds "$SECS" "$@"
