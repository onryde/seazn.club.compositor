#!/bin/sh
# The exec-time witness of a CEF child's environment (C12; the PR #36 final review's I-1). TEST ONLY:
# it is never in the relay image. `scripts/image_probe.py` bind-mounts this file into ONE dedicated
# container and points gstcefsrc's GST_CEF_SUBPROCESS_PATH at it. gstcefsrc (gstcefsrc.cc at the
# F-001 pin) then hands CEF this path as `browser_subprocess_path` and puts the real gstcefsubprocess
# in CEF_SUBPROCESS_PATH, so CEF execs this script for every child it launches (the zygotes and the
# network service); the zygotes' forks (the renderer, the GPU process) keep the environment their
# zygote was exec'd with.
#
# Why not `/proc/<pid>/environ`: Chromium rewrites that area for its process title (F-020: all NUL,
# or the title), so a read of it witnesses nothing, and a deleted `remove_var` survived it.
#
# One file per exec, named by the pid the child keeps after `exec`. It records only whether each
# variable is SET, never a value: JOB_TOKEN (must be unset), and three controls that must be set:
# DISPLAY (the image's ENV), SESSION_ID (passed to the container beside JOB_TOKEN) and
# CEF_SUBPROCESS_PATH (set inside the compositor after its exec, where `remove_var` acts too).
dir=/tmp/seazn-cef-env-witness
type=none
for a in "$@"; do
  case $a in --type=*) type=${a#--type=} ;; esac
done
j=${JOB_TOKEN+set} d=${DISPLAY+set} s=${SESSION_ID+set} c=${CEF_SUBPROCESS_PATH+set}
{ mkdir -p "$dir" && printf 'pid=%s type=%s JOB_TOKEN=%s DISPLAY=%s SESSION_ID=%s CEF_SUBPROCESS_PATH=%s\n' \
    "$$" "$type" "${j:-unset}" "${d:-unset}" "${s:-unset}" "${c:-unset}" >"$dir/$$"; } 2>/dev/null || true
if [ -z "${CEF_SUBPROCESS_PATH:-}" ]; then
  echo "cef_env_witness: no CEF_SUBPROCESS_PATH to exec" >&2
  exit 70
fi
exec "$CEF_SUBPROCESS_PATH" "$@"
