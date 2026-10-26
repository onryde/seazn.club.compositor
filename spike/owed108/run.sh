#!/usr/bin/env bash
# Owed 108: one part of owed108.py on this Machine. Its lines go to the platform's log and to
# /out/<part>.jsonl; the exit code to /out/<part>.exit. The Machine then waits to be read and destroyed.
# The two SPIKE_A_SRT_* URLs arrive as the app's secrets (environment); nothing here prints them.
set -u
mkdir -p /out
part="$1"
shift
unset GST_DEBUG
python3 -u /owed108/owed108.py run --part "$part" "$@" 2>&1 | tee "/out/$part.jsonl"
echo "EXIT=${PIPESTATUS[0]}" | tee "/out/$part.exit"
exec sleep infinity
