#!/usr/bin/env bash
# Runs INSIDE the compositor's container (C14), as root, from run-case.sh's `in_compositor`:
#
#   docker exec -i -u 0 <compositor> bash -s -- <command> [args] < harness/in-container.sh
#
# The relay image is never changed for the harness (the dispatch's ruling: no package is added to
# the shipped image), and it carries no procps, so everything here reads /proc with bash builtins:
# no pkill, no ps. A process's type is its `--type=` argument (CEF's children: zygote, gpu-process,
# renderer, utility; F-013), its parent and start time come from /proc/<pid>/stat. Processes run
# from here (the burn's workers) are in the container's cgroup, as every `docker exec` is.
#
# Every line printed is one JSON object, stamped with this host's wall clock in ms (the kernel's
# CLOCK_REALTIME, the clock `docker logs --timestamps` and `docker events` use). A kill prints its
# targets BEFORE it kills (`at_ms`: the container may die under the exec before anything after),
# then its status (`rc_at_ms`, `rc`).
#
# Commands:
#   tree                       one line per process: pid, ppid, start_ms, comm, type (or null);
#                              start_ms is on the same wall clock (`boot_ms` plus the process's
#                              start ticks), so it compares with docker's events and the kill marks
#   kill-cef                   SIGKILL every CEF child at once (case 8); fails when it finds none
#   kill-compositor            SIGKILL the compositor process (case 9)
#   kill-zygote gpu|renderer   SIGKILL the compositor's zygote that parents the GPU process, or a
#                              renderer (cases 13 and 14); prints the zygote's children first
#   gpu-loop N SPACING_MS DEADLINE_MS
#                              SIGKILL the GPU process N times, each once a NEW pid has replaced the
#                              last and at least SPACING_MS after the previous kill; SPACING_MS 0 is
#                              the fast loop (case 15): no wait but a 50 ms look-again while no new
#                              pid is there. Ends when N are done, when the compositor is gone (the
#                              exec dies with its container), or at DEADLINE_MS (this host's epoch
#                              ms), and then FAILS (`timed_out`): no leg may wait out the relay's own
#                              deadline. Prints one `done_at_ms` line at its end
#   burn WORKERS SECONDS       WORKERS busy loops for SECONDS each, in this container's cgroup (case
#                              6); prints its start (`at_ms`) and, once every worker has stopped
#                              itself, its end (`done_at_ms`, `elapsed_ms`)
#
# A pid listed can be gone by the time it is read (this script's own command substitutions are in
# every listing). Every /proc read is a builtin `read`/`mapfile` with its error redirect FIRST and a
# status test: under `set -e`, bash 5.2's `$(<file)` on a missing file ends the script silently even
# inside `|| return 1` (fix round 2: CI's `tree` stopped at the first vanished pid and `kill-zygote`
# printed nothing, 6 of 6 legs).
#
# IN_CONTAINER_PROC moves /proc (the harness's own test, crates/harness/tests/in_container.rs, runs
# this on a fake tree); the cases never set it.
set -euo pipefail

PROC="${IN_CONTAINER_PROC:-/proc}"

now_ms() {
  local t=$EPOCHREALTIME
  t=${t//[.,]/}
  echo $((10#$t / 1000))
}

CLK_TCK=$(getconf CLK_TCK)

# The boot instant on this host's wall clock, in ms: the wall clock now, less /proc/uptime read at
# the same moment (both kernel clocks; uptime's resolution is 10 ms). Never /proc/stat's btime
# (fix round 3): it is whole seconds, fixed at boot while the wall clock is slewed, and CI's case
# 14 read it 173 to 1 080 ms early against docker's events, so every process of the restarted
# container looked older than the restart's `start` (6 of 6 legs).
boot_ms() {
  local up sec frac
  IFS=' ' read -r up _ 2>/dev/null <"$PROC/uptime" || return 1
  sec=${up%%.*}
  frac=${up#*.}
  [ "$frac" != "$up" ] || frac=0
  frac="${frac}000"
  frac=${frac:0:3}
  echo $(($(now_ms) - (10#$sec * 1000 + 10#$frac)))
}
if ! BOOT_MS=$(boot_ms); then
  echo '{"error":"no /proc/uptime"}'
  exit 1
fi

# The --type= argument of a process (empty when none), read with builtins only. The product's own
# rule (`split_cmdline` in crates/adapters/src/cef_graphics.rs, mirrored, never imported): the
# command line splits on NUL AND on whitespace, because Chromium's renamed children (setproctitle)
# hold ONE space-joined string, padded with NULs. A NUL-only split finds no `--type=` in them (the
# PR #52 review's C1). The first argument that starts with `--type=` wins (`flag_value`).
type_of() {
  local field a fields=() words=()
  mapfile -d '' -t fields 2>/dev/null <"$PROC/$1/cmdline" || return 0
  for field in "${fields[@]}"; do
    IFS=$' \t\n\r\f\v' read -r -d '' -a words <<<"$field" || true
    for a in "${words[@]}"; do
      if [[ $a == --type=* ]]; then
        printf '%s' "${a#--type=}"
        return 0
      fi
    done
  done
}

# One line of a /proc file into REPLY_LINE; fails when the process is gone.
read_line() {
  REPLY_LINE=""
  IFS= read -r REPLY_LINE 2>/dev/null <"$1" || [ -n "$REPLY_LINE" ]
}

# Sets STAT_PPID and STAT_START_MS for a pid (fields after the comm's closing parenthesis); fails
# when the process is gone.
read_stat() {
  local s f=()
  read_line "$PROC/$1/stat" || return 1
  s=${REPLY_LINE##*) }
  read -r -a f <<<"$s" || true
  [ "${#f[@]}" -ge 20 ] || return 1
  STAT_PPID=${f[1]}
  STAT_START_MS=$((BOOT_MS + f[19] * 1000 / CLK_TCK))
}

pids() {
  local d
  for d in "$PROC"/[0-9]*; do
    [ -d "$d" ] && printf '%s\n' "${d#"$PROC"/}"
  done
  return 0
}

compositor_pid() {
  local p c
  for p in $(pids); do
    read_line "$PROC/$p/comm" || continue
    c=$REPLY_LINE
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

# The kill's status, after it: `rc_at_ms`, never `at_ms` (run-case.sh's mark_lines keeps them apart).
print_rc() {
  printf '{"rc_at_ms":%s,"rc":%s}\n' "$(now_ms)" "$1"
}

cmd_tree() {
  local p c t
  for p in $(pids); do
    read_stat "$p" || continue
    read_line "$PROC/$p/comm" || continue
    c=${REPLY_LINE//[^A-Za-z0-9._ -]/?}
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
  printf '{"at_ms":%s,"killed":%s,"types":%s}\n' "$(now_ms)" "$(json_list "${victims[@]}")" \
    "$(json_strs "${types[@]}")"
  if [ "${#victims[@]}" -eq 0 ]; then
    print_rc 1
    exit 1
  fi
  kill -KILL "${victims[@]}" || rc=$?
  print_rc "$rc"
  [ "$rc" -eq 0 ]
}

cmd_kill_compositor() {
  local p rc=0
  p=$(compositor_pid)
  printf '{"at_ms":%s,"pid":%s}\n' "$(now_ms)" "${p:-null}"
  if [ -z "$p" ]; then
    print_rc 1
    exit 1
  fi
  kill -KILL "$p" || rc=$?
  print_rc "$rc"
  [ "$rc" -eq 0 ]
}

cmd_kill_zygote() {
  local want="$1" comp z c chosen="" child_types=() kids=() rc=0
  case "$want" in
    gpu) want=gpu-process ;;
    renderer) want=renderer ;;
    *)
      echo '{"error":"kill-zygote gpu|renderer"}'
      exit 64
      ;;
  esac
  comp=$(compositor_pid)
  if [ -z "$comp" ]; then
    printf '{"at_ms":%s,"pid":null,"want":"%s","error":"no compositor process"}\n' "$(now_ms)" "$want"
    exit 1
  fi
  for z in $(pids_of_type zygote); do
    read_stat "$z" || continue
    [ "$STAT_PPID" = "$comp" ] || continue
    kids=()
    child_types=()
    for c in $(pids); do
      read_stat "$c" || continue
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
    printf '{"at_ms":%s,"pid":null,"want":"%s"}\n' "$(now_ms)" "$want"
    exit 1
  fi
  local ztype zppid
  ztype=$(type_of "$chosen")
  zppid=null
  if read_stat "$chosen"; then zppid=$STAT_PPID; fi
  printf '{"at_ms":%s,"pid":%s,"ppid":%s,"compositor":%s,"type":"%s","want":"%s","children":%s,"child_types":%s}\n' \
    "$(now_ms)" "$chosen" "$zppid" "$comp" "$ztype" "$want" "$(json_list "${kids[@]}")" \
    "$(json_strs "${child_types[@]}")"
  kill -KILL "$chosen" || rc=$?
  print_rc "$rc"
  [ "$rc" -eq 0 ]
}

cmd_gpu_loop() {
  local n="$1" spacing="$2" deadline="$3" last="" kills=0 p t at prev_at=0 comp timed_out=false
  comp=$(compositor_pid)
  while [ "$kills" -lt "$n" ]; do
    [ -n "$comp" ] && [ -d "$PROC/$comp" ] || break
    if [ "$(now_ms)" -ge "$deadline" ]; then
      timed_out=true
      break
    fi
    p=""
    for t in $(pids_of_type gpu-process); do
      p=$t
      break
    done
    if [ -z "$p" ] || [ "$p" = "$last" ]; then
      sleep 0.05
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
  local alive=false
  if [ -n "$comp" ] && [ -d "$PROC/$comp" ]; then alive=true; fi
  printf '{"done_at_ms":%s,"done":%s,"wanted":%s,"compositor_alive":%s,"timed_out":%s}\n' \
    "$(now_ms)" "$kills" "$n" "$alive" "$timed_out"
  # Success is N kills, or the compositor gone after at least one (it exited under the loop).
  [ "$timed_out" = false ] || exit 1
  [ "$kills" -eq "$n" ] || { [ "$alive" = false ] && [ "$kills" -ge 1 ]; }
}

cmd_burn() {
  local workers="$1" seconds="$2" i start
  if ! [[ "$workers" =~ ^[1-9][0-9]*$ && "$seconds" =~ ^[1-9][0-9]*$ ]]; then
    echo '{"error":"burn WORKERS SECONDS"}'
    exit 64
  fi
  start=$(now_ms)
  printf '{"at_ms":%s,"workers":%s,"seconds":%s}\n' "$start" "$workers" "$seconds"
  # Each worker stops itself at its own deadline (EPOCHREALTIME, no fork per turn): an exec killed
  # with its container leaves no spin.
  local end=$((start + seconds * 1000))
  for ((i = 0; i < workers; i++)); do
    (
      t=${EPOCHREALTIME//[.,]/}
      while ((10#$t / 1000 < end)); do t=${EPOCHREALTIME//[.,]/}; done
    ) &
  done
  wait
  local done_at
  done_at=$(now_ms)
  printf '{"done_at_ms":%s,"workers":%s,"elapsed_ms":%s}\n' "$done_at" "$workers" $((done_at - start))
}

case "${1:-}" in
  tree) cmd_tree ;;
  kill-cef) cmd_kill_cef ;;
  kill-compositor) cmd_kill_compositor ;;
  kill-zygote) cmd_kill_zygote "${2:?gpu|renderer}" ;;
  gpu-loop) cmd_gpu_loop "${2:?N}" "${3:?SPACING_MS}" "${4:?DEADLINE_MS}" ;;
  burn) cmd_burn "${2:?WORKERS}" "${3:?SECONDS}" ;;
  *)
    echo '{"error":"usage: tree | kill-cef | kill-compositor | kill-zygote gpu|renderer | gpu-loop N SPACING_MS DEADLINE_MS | burn WORKERS SECONDS"}'
    exit 64
    ;;
esac
