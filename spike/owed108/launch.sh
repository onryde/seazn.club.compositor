#!/usr/bin/env bash
# launch.sh <machine-name> <part> [owed108.py run args...]   (Owed 108, F-032)
# One fresh performance-4x Machine in seazn-relay-spike (lhr) on the C00 image, with owed108.py and run.sh
# uploaded as files. The Machine's config holds no secret: the SPIKE_A_* URLs are the app's own secrets and
# reach the process as its environment. Read the results with
#   fly machine exec <id> 'cat /out/<part>.jsonl' -a seazn-relay-spike
# then destroy the Machine (fly machine destroy <id> -a seazn-relay-spike --force). Flags go before the
# image and the command after `--`, or flyctl takes the probe's --trials for its own.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
name="$1"
shift
date -u +%FT%T
fly machine run -a seazn-relay-spike -r lhr --name "$name" --restart no --detach \
  --vm-cpu-kind performance --vm-cpus 4 --vm-memory 8192 \
  --file-local "/owed108/owed108.py=$here/owed108.py" --file-local "/owed108/run.sh=$here/run.sh" \
  registry.fly.io/seazn-relay-spike:deployment-01M4ZBW92TN0GPMPC9S0KK5TQ3 -- bash /owed108/run.sh "$@"
