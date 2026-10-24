#!/usr/bin/env bash
# Runs INSIDE the compositor's container (C14), as root, from run-case.sh's `in_compositor`:
#
#   docker exec -i -u 0 <compositor> bash -s -- <command> [args] < harness/in-container.sh
#
# The relay image is never changed for the harness (the dispatch's ruling: no package is added to
# the shipped image), and it carries no procps, so everything here reads /proc with bash builtins:
# no pkill, no ps. A process's type is its `--type=` argument (CEF's children: zygote, gpu-process,
# renderer, utility; F-013), its parent and start time come from /proc/<pid>/stat.
#
# Every line printed is one JSON object, stamped with this host's wall clock in ms (the kernel's
# CLOCK_REALTIME, the clock `docker logs --timestamps` and `docker events` use).
#
# Commands:
#   tree                       one line per process: pid, ppid, start_ms, comm, type (or null)
#   kill-cef                   SIGKILL every CEF child at once (case 8)
#   kill-compositor            SIGKILL the compositor process (case 9)
#   kill-zygote gpu|renderer   SIGKILL the compositor's zygote that parents the GPU process, or a
#                              renderer (cases 13 and 14); prints the zygote's children first
#   gpu-loop N SPACING_MS      SIGKILL the GPU process N times, each once a NEW pid has replaced the
#                              last and at least SPACING_MS after the previous kill; SPACING_MS 0
#                              is the fast loop (case 15): a tight /proc loop, no wait. Stops when
#                              the compositor is gone (the exec dies with its container)
set -euo pipefail

PROC=/proc

now_ms() {
  local t=$EPOCHREALTIME
  t=${t//[.,]/}
  echo $((10#$t / 1000))
}

CLK_TCK=$(getconf CLK_TCK)
BTIME=$(awk '/^btime/ { print $2 }' "$PROC/stat")

# The --type= argument of a process (empty when none), read with builtins only.
type_of() {
  local a args=()
  mapfile -d '' -t args <"$PROC/$1/cmdline" 2>/dev/null || return 0
  for a in "${args[@]}"; do
    if [[ $a == --type=* ]]; then
      printf '%s' "${a#--type=}"
      return 0
    fi
  done
}

# Sets STAT_PPID and STAT_START_MS for a pid (fields after the comm's closing parenthesis).
read_stat() {
  local s f
  { s=$(<"$PROC/$1/stat"); } 2>/dev/null || return 1
  s=${s##*) }
  read -r -a f <<<"$s"
  STAT_PPID=${f[1]}
  STAT_START_MS=$((BTIME * 1000 + f[19] * 1000 / CLK_TCK))
}

pids() {
  local d
  for d in "$PROC"/[0-9]*; do printf '%s\n' "${d#"$PROC"/}"; done
}

compositor_pid() {
  local p c
  for p in $(pids); do
    { c=$(<"$PROC/$p/comm"); } 2>/dev/null || continue
    if [ "$c" = compositor ]; then
      printf '%s' "$p"
      return 0
    fi
  done
}

pids_of_type() {
  local p
  for p in $(pids); do
    [ "$(type_of "$p")" = "$1" ] && printf '%s\n' "$p"
  done
  return 0
}

json_list() {
  local out="" x
  for x in "$@"; do out="${out:+$out,}$x"; done
  printf '[%s]' "$out"
}

json_strs() {
  local out="" x
  for x in "$@"; do out="${out:+$out,}\"$x\""; done
  printf '[%s]' "$out"
}

cmd_tree() {
  local p c t
  for p in $(pids); do
    read_stat "$p" 2>/dev/null || continue
    { c=$(<"$PROC/$p/comm"); } 2>/dev/null || continue
    c=${c//[^A-Za-z0-9._ -]/?}
    t=$(type_of "$p")
    if [ -n "$t" ]; then t="\"$t\""; else t=null; fi
    printf '{"at_ms":%s,"pid":%s,"ppid":%s,"start_ms":%s,"comm":"%s","type":%s}\n' \
      "$(now_ms)" "$p" "$STAT_PPID" "$STAT_START_MS" "$c" "$t"
  done
}

cmd_kill_cef() {
  local p t victims=() types=() rc=0
  for p in $(pids); do
    t=$(type_of "$p")
    if [ -n "$t" ]; then
      victims+=("$p")
      types+=("$t")
    fi
  done
  local at
  at=$(now_ms)
  if [ "${#victims[@]}" -gt 0 ]; then kill -KILL "${victims[@]}" || rc=$?; else rc=1; fi
  printf '{"at_ms":%s,"killed":%s,"types":%s,"rc":%s}\n' "$at" "$(json_list "${victims[@]}")" \
    "$(json_strs "${types[@]}")" "$rc"
}

cmd_kill_compositor() {
  local p rc=0 at
  p=$(compositor_pid)
  at=$(now_ms)
  if [ -n "$p" ]; then kill -KILL "$p" || rc=$?; else rc=1; fi
  printf '{"at_ms":%s,"pid":%s,"rc":%s}\n' "$at" "${p:-null}" "$rc"
}

cmd_kill_zygote() {
  local want="$1" comp z c chosen="" child_types=() kids=() rc=0 at
  case "$want" in
    gpu) want=gpu-process ;;
    renderer) want=renderer ;;
    *) echo '{"error":"kill-zygote gpu|renderer"}'; exit 64 ;;
  esac
  comp=$(compositor_pid)
  [ -n "$comp" ] || { echo '{"error":"no compositor process"}'; exit 1; }
  for z in $(pids_of_type zygote); do
    read_stat "$z" 2>/dev/null || continue
    [ "$STAT_PPID" = "$comp" ] || continue
    kids=()
    child_types=()
    for c in $(pids); do
      read_stat "$c" 2>/dev/null || continue
      if [ "$STAT_PPID" = "$z" ]; then
        kids+=("$c")
        child_types+=("$(type_of "$c")")
      fi
    done
    local t
    for t in "${child_types[@]}"; do
      if [ "$t" = "$want" ]; then
        chosen=$z
        break 2
      fi
    done
  done
  if [ -z "$chosen" ]; then
    printf '{"at_ms":%s,"pid":null,"want":"%s","rc":1}\n' "$(now_ms)" "$want"
    exit 1
  fi
  local ztype
  ztype=$(type_of "$chosen")
  read_stat "$chosen"
  local zppid=$STAT_PPID
  at=$(now_ms)
  kill -KILL "$chosen" || rc=$?
  printf '{"at_ms":%s,"pid":%s,"ppid":%s,"compositor":%s,"type":"%s","want":"%s","children":%s,"child_types":%s,"rc":%s}\n' \
    "$at" "$chosen" "$zppid" "$comp" "$ztype" "$want" "$(json_list "${kids[@]}")" \
    "$(json_strs "${child_types[@]}")" "$rc"
}

cmd_gpu_loop() {
  local n="$1" spacing="$2" last="" kills=0 p t at prev_at=0 comp
  comp=$(compositor_pid)
  while [ "$kills" -lt "$n" ]; do
    [ -n "$comp" ] && [ -d "$PROC/$comp" ] || break
    p=""
    for t in $(pids_of_type gpu-process); do p=$t; break; done
    if [ -z "$p" ] || [ "$p" = "$last" ]; then
      [ "$spacing" -gt 0 ] && sleep 0.05
      continue
    fi
    if [ "$spacing" -gt 0 ]; then
      at=$(now_ms)
      if [ $((at - prev_at)) -lt "$spacing" ]; then
        sleep 0.05
        continue
      fi
    fi
    at=$(now_ms)
    if kill -KILL "$p" 2>/dev/null; then
      kills=$((kills + 1))
      printf '{"at_ms":%s,"pid":%s,"n":%s}\n' "$at" "$p" "$kills"
      prev_at=$at
      last=$p
    fi
  done
  printf '{"done_at_ms":%s,"done":%s,"compositor_alive":%s}\n' "$(now_ms)" "$kills" \
    "$([ -n "$comp" ] && [ -d "$PROC/$comp" ] && echo true || echo false)"
}

case "${1:-}" in
  tree) cmd_tree ;;
  kill-cef) cmd_kill_cef ;;
  kill-compositor) cmd_kill_compositor ;;
  kill-zygote) cmd_kill_zygote "${2:?gpu|renderer}" ;;
  gpu-loop) cmd_gpu_loop "${2:?N}" "${3:?SPACING_MS}" ;;
  *)
    echo '{"error":"usage: tree | kill-cef | kill-compositor | kill-zygote gpu|renderer | gpu-loop N SPACING_MS"}'
    exit 64
    ;;
esac
