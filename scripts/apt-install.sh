#!/usr/bin/env bash
# Installs apt packages on a GitHub-hosted Ubuntu runner without letting a slow mirror eat the job
# (decision log "CI: apt bounded on a trickling mirror", 2026-10-23; tests: scripts/apt_install_test.py).
#
# Why: on four check runs that day, azure.archive.ubuntu.com (the runner's first mirror) served the
# GStreamer packages at about 40 to 80 kB/s, with gaps of up to 5 minutes between files, against 7.9
# to 22.6 MB/s on a normal run. The step ran 22 to 25 minutes and the job's 25-minute bound cancelled
# the run before cargo test. apt moves to the mirror list's next entry only on an error, and a
# trickle is not one; apt's Timeout bounds only silence.
#
# So: short network timeouts and retries for each file; each network phase is an attempt under a
# wall-clock bound; after a failed attempt, azure is dropped from the runner's mirror list, so later
# attempts fetch from the list's next entry (archive.ubuntu.com). The packages are fetched with
# --download-only, which a kill cannot leave half installed (dpkg never runs), and are then
# installed from the local cache. A finished .deb stays in /var/cache/apt/archives, so a retry does
# not fetch it again. Worst case over the network phases: 2 x (45 + 5) + 5 + 3 x (120 + 5) + 2 x 5
# = 490 s, inside the GStreamer step's 10-minute bound with room for the install (20 to 34 s).
#
# Usage: scripts/apt-install.sh <package>...
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "::error::apt-install.sh: no packages named"
  exit 2
fi

mirrors=${APT_MIRRORS:-/etc/apt/apt-mirrors.txt}
opts=(-o Acquire::Retries=5 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20)

if [ -f "$mirrors" ]; then
  echo "apt-install: mirror list $mirrors:"
  cat "$mirrors"
fi

# Drops azure from the mirror list, unless it is absent or the only entry.
fall_back() {
  if [ ! -f "$mirrors" ]; then
    echo "apt-install: no mirror list at $mirrors, nothing to fall back to"
    return 0
  fi
  local rest
  rest=$(grep -v 'azure\.archive\.ubuntu\.com' "$mirrors" || true)
  if [ -z "$rest" ] || [ "$rest" = "$(cat "$mirrors")" ]; then
    echo "apt-install: no azure entry with another mirror beside it in $mirrors; kept as is"
    return 0
  fi
  printf '%s\n' "$rest" | sudo tee "$mirrors" > /dev/null
  echo "apt-install: dropped azure.archive.ubuntu.com from $mirrors; now:"
  cat "$mirrors"
}

# attempt <what> <tries> <seconds per try> <apt-get arguments>...
attempt() {
  local what=$1 tries=$2 bound=$3 i=1 start rc
  shift 3
  while :; do
    start=$SECONDS
    rc=0
    sudo timeout -k 5 "$bound" apt-get "${opts[@]}" "$@" || rc=$?
    if [ "$rc" -eq 0 ]; then
      echo "apt-install: $what done in $((SECONDS - start)) s (attempt $i of $tries)"
      return 0
    fi
    echo "::warning::apt $what: attempt $i of $tries failed after $((SECONDS - start)) s (exit $rc; 124 is its ${bound} s bound)"
    if [ "$i" -ge "$tries" ]; then
      echo "::error::apt $what failed $tries times: a mirror is down or trickling (docs/_INDEX.md, 2026-10-23)"
      return 1
    fi
    fall_back
    i=$((i + 1))
    sleep 5
  done
}

attempt "index update" 2 45 update
attempt "download" 3 120 install -y --no-install-recommends --download-only "$@"
# Offline now: every package is in the cache. A kill could leave dpkg half done, so the only bound
# here is the step's own.
sudo apt-get "${opts[@]}" install -y --no-install-recommends "$@"
