# Sourced by every run-*.sh (C00 spike, THROWAWAY). Plain-docker portable: no Fly paths.
# Config is env-only: C00_OUT (log dir), DISPLAY, GST_PLUGIN_PATH, SPIKE_* (secrets, never echoed).
set -u
OUT=${C00_OUT:-/tmp/c00}
mkdir -p "$OUT" && chmod 777 "$OUT"
export GST_PLUGIN_PATH=${GST_PLUGIN_PATH:-/opt/gstcefsrc}
export DISPLAY=${DISPLAY:-:99}
export GST_DEBUG_NO_COLOR=1
SPIKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# Run media as the non-root user when we are root (the production image is non-root, C12).
if [ "$(id -u)" = 0 ] && id spike >/dev/null 2>&1; then AS=(runuser -u spike --); else AS=(); fi
start_xvfb() {
  if ! pgrep -x Xvfb >/dev/null; then
    Xvfb "${DISPLAY}" -screen 0 1920x1080x24 -nolisten tcp -ac >/dev/null 2>&1 &
    sleep 1
  fi
}
# All output of the calling script goes through the redactor before it reaches a terminal or file.
redact_all() { exec > >(python3 -u "$SPIKE_DIR/redact.py" | tee -a "$OUT/$1") 2>&1; }
secret_presence() { for v in SPIKE_A_SRT_PLAY_URL SPIKE_A_SRT_PUSH_URL SPIKE_A_RTMPS_PUSH_URL SPIKE_B_RTMPS_URL; do if [ -n "${!v:-}" ]; then echo "$v=set"; else echo "$v=UNSET"; fi; done; }
raw_to_png() { # raw_to_png <in> <format bgra|ayuv> <w> <h> <out.png>
  gst-launch-1.0 -q filesrc location="$1" blocksize=$(( $3 * $4 * 4 )) ! rawvideoparse format="$2" width="$3" height="$4" framerate=1/1 \
    ! videoconvert ! videoscale ! video/x-raw,width=960,height=540 ! pngenc ! filesink location="$5" >/dev/null 2>&1 && echo "png $5"
}
