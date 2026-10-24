#!/usr/bin/env bash
# Runs INSIDE the compositor Machine for C15's cost legs, as root, sent there by leg.sh (flyctl ssh
# sftp) and run over `flyctl ssh console -C` / `flyctl machine exec`. The relay image is never
# changed for the harness and carries no procps or python, so this reads /proc with bash builtins,
# as harness/in-container.sh does (whose rules it mirrors: a process's type is its `--type=`
# argument, the command line split on NUL and on whitespace).
#
#   in-compositor.sh sample <max_s>   one JSON line a second for at most max_s seconds (F-005's method:
#                                     the whole Machine from /proc/stat's first line, and the process
#                                     tree from each /proc/<pid>/stat's utime + stime)
#   in-compositor.sh kill-renderer    SIGKILL every CEF renderer (`--type=renderer`), once: the page's
#                                     renderer dies and cefsrc reloads the page (its
#                                     OnRenderProcessTerminated). Never the GPU process or a zygote:
#                                     killing every CEF child aborts the compositor (Owed 62), which
#                                     is a process restart, not a page reload. Prints its targets
#                                     before it kills (`at_ms`), then its status (`rc_at_ms`, `rc`);
#                                     fails when it finds none
#
# A sample line (cores are CPU-seconds a second, three decimals):
#
#   {"t_ms", "dt_ms", "ncpu", "machine", "tree", "cats": {compositor, renderer, gpu, utility, zygote,
#    cef_other, xvfb, self, other}, "renderers": [{pid, client, cores}], "top_thread": {name, cores},
#    "mem_used_mb", "procs"}
#
# machine: busy jiffies over all jiffies between two reads, times ncpu (busy = all but idle and
# iowait). tree: the product's processes (the compositor, its CEF children and Xvfb). self: this
# script. other: the rest (Fly's init, hallpass and the ssh session). top_thread: the compositor's
# busiest thread (a single-thread bottleneck drops frames below the Machine's p95; the brief's I4).
# A process seen for the first time counts its whole CPU time (it started within the interval).
# The first read prints nothing (no interval yet).
#
# COST_PROC_SEQ (test-cost.sh only) replaces /proc with numbered trees: read k uses
# $COST_PROC_SEQ/k, and its clock is that tree's `now_ms`; COST_SELF_PID names the sampler's pid
# there. COST_PROC moves /proc to one fake tree on the real clock (the cadence's test). The Machine
# sets none of them.
set -euo pipefail

CLK_TCK="${COST_CLK_TCK:-$(getconf CLK_TCK)}"
PROC="${COST_PROC:-/proc}"
NOW_MS=0

clock() {
  if [ -n "${COST_PROC_SEQ:-}" ]; then
    IFS= read -r NOW_MS <"$PROC/now_ms"
  else
    local t=$EPOCHREALTIME
    t=${t//[.,]/}
    NOW_MS=$((10#$t / 1000))
  fi
}

REPLY_LINE=""
read_line() {
  REPLY_LINE=""
  IFS= read -r REPLY_LINE 2>/dev/null <"$1" || [ -n "$REPLY_LINE" ]
}

# TYPE and CLIENT of a pid: its --type= and --renderer-client-id= arguments (empty when none). The
# product's split (crates/adapters/src/cef_graphics.rs `split_cmdline`, mirrored): Chromium's renamed
# children hold ONE space-joined string, padded with NULs.
TYPE=""
CLIENT=""
type_of() {
  local field a fields=() words=()
  TYPE=""
  CLIENT=""
  mapfile -d '' -t fields 2>/dev/null <"$PROC/$1/cmdline" || return 0
  for field in "${fields[@]}"; do
    IFS=$' \t\n\r\f\v' read -r -d '' -a words <<<"$field" || true
    for a in "${words[@]}"; do
      if [[ -z $TYPE && $a == --type=* ]]; then TYPE=${a#--type=}; fi
      if [[ -z $CLIENT && $a == --renderer-client-id=* ]]; then CLIENT=${a#--renderer-client-id=}; fi
    done
  done
  [[ $CLIENT =~ ^[0-9]+$ ]] || CLIENT=""
  return 0
}

# Sets ST_PPID, ST_TICKS (utime + stime) and ST_START (start ticks after boot) from a stat line;
# fails on a short one. The fields after the comm's closing parenthesis.
ST_PPID=0
ST_TICKS=0
ST_START=0
parse_stat() {
  local s f=()
  s=${1##*) }
  read -r -a f <<<"$s" || true
  [ "${#f[@]}" -ge 20 ] || return 1
  ST_PPID=${f[1]}
  ST_TICKS=$((f[11] + f[12]))
  ST_START=${f[19]}
}

# Integer milli-cores as a decimal: 1234 -> 1.234.
dec() {
  local v=$1 sign=""
  if [ "$v" -lt 0 ]; then sign="-"; v=$((-v)); fi
  printf -v DEC '%s%d.%03d' "$sign" $((v / 1000)) $((v % 1000))
}
DEC=""
# Ticks over dt_ms as milli-cores.
mcores() { MC=$(($1 * 1000000 / (CLK_TCK * $2))); }
MC=0

# A thread's name as a JSON string's body: a quote, a backslash or a control character becomes ?.
json_name() {
  NAME=${1//[\"\\]/?}
  NAME=${NAME//[[:cntrl:]]/?}
}
NAME=""

cmd_sample() {
  local max_s="$1" k=0 sleep_fd fifo
  [[ $max_s =~ ^[0-9]+$ ]] || { echo '{"error":"sample <max_s>"}'; exit 64; }
  if [ -z "${COST_PROC_SEQ:-}" ]; then
    # A builtin sleep: a read with a timeout on a FIFO opened read-write (it never sees EOF).
    fifo="/tmp/seazn-cost-sleep.$$"
    mkfifo "$fifo"
    exec {sleep_fd}<>"$fifo"
    rm -f "$fifo"
  fi
  declare -A prev=() cat_of=() client_of=() tprev=() tname=()
  local self=${COST_SELF_PID:-$$} ncpu=0 line key p d dd c comp prev_busy=0 prev_total=0 prev_ms=0 t0=0 have_prev=0
  local u n s i w q sq st rest busy total dt_ms mt mem_total mem_avail lk lv procs
  local -A sum=()
  local cats=(compositor renderer gpu utility zygote cef_other xvfb self other)
  while :; do
    if [ -n "${COST_PROC_SEQ:-}" ]; then
      PROC="$COST_PROC_SEQ/$k"
      [ -d "$PROC" ] || break
    fi
    clock
    [ "$t0" -ne 0 ] || t0=$NOW_MS
    # The whole Machine.
    read -r _ u n s i w q sq st rest <"$PROC/stat"
    busy=$((u + n + s + q + sq + ${st:-0}))
    total=$((busy + i + w))
    if [ "$ncpu" -eq 0 ]; then
      while read -r line; do
        if [[ $line =~ ^cpu[0-9]+[[:space:]] ]]; then ncpu=$((ncpu + 1)); fi
      done <"$PROC/stat"
    fi
    mem_total=0
    mem_avail=0
    while read -r lk lv _; do
      case "$lk" in
        MemTotal:) mem_total=$lv ;;
        MemAvailable:) mem_avail=$lv ;;
      esac
    done <"$PROC/meminfo"
    # The processes.
    for c in "${cats[@]}"; do sum[$c]=0; done
    local -A seen=()
    local renderers="" comp=""
    procs=0
    for d in "$PROC"/[0-9]*; do
      p=${d##*/}
      read_line "$d/stat" || continue
      parse_stat "$REPLY_LINE" || continue
      key="$p:$ST_START"
      seen[$key]=1
      procs=$((procs + 1))
      if [ -z "${cat_of[$key]:-}" ]; then
        read_line "$d/comm" || continue
        c=$REPLY_LINE
        type_of "$p"
        if [ "$c" = compositor ]; then cat_of[$key]=compositor
        elif [ -n "$TYPE" ]; then
          case "$TYPE" in
            renderer) cat_of[$key]=renderer ;;
            gpu-process) cat_of[$key]=gpu ;;
            utility) cat_of[$key]=utility ;;
            zygote) cat_of[$key]=zygote ;;
            *) cat_of[$key]=cef_other ;;
          esac
        elif [ "$p" = "$self" ]; then cat_of[$key]=self
        elif [ "$c" = Xvfb ]; then cat_of[$key]=xvfb
        else cat_of[$key]=other
        fi
        client_of[$key]=$CLIENT
      fi
      [ "${cat_of[$key]}" != compositor ] || comp=$p
      if [ "$have_prev" -eq 1 ]; then
        dd=$((ST_TICKS - ${prev[$key]:-0}))
        [ "$dd" -ge 0 ] || dd=0
        sum[${cat_of[$key]}]=$((${sum[${cat_of[$key]}]} + dd))
        if [ "${cat_of[$key]}" = renderer ]; then
          dt_ms=$((NOW_MS - prev_ms))
          [ "$dt_ms" -gt 0 ] || dt_ms=1
          mcores "$dd" "$dt_ms"
          dec "$MC"
          renderers="${renderers:+$renderers,}{\"pid\":$p,\"client\":${client_of[$key]:-null},\"cores\":$DEC}"
        fi
      fi
      prev[$key]=$ST_TICKS
    done
    for key in "${!prev[@]}"; do
      [ -n "${seen[$key]:-}" ] || unset 'prev[$key]' 'cat_of[$key]' 'client_of[$key]'
    done
    # The compositor's busiest thread.
    local top_name="" top_d=-1 tid tkey
    if [ -n "$comp" ]; then
      local -A tseen=()
      for d in "$PROC/$comp/task"/[0-9]*; do
        tid=${d##*/}
        read_line "$d/stat" || continue
        parse_stat "$REPLY_LINE" || continue
        tkey="$tid:$ST_START"
        tseen[$tkey]=1
        if [ -z "${tname[$tkey]:-}" ]; then
          read_line "$d/comm" || continue
          json_name "$REPLY_LINE"
          tname[$tkey]=$NAME
        fi
        if [ "$have_prev" -eq 1 ]; then
          dd=$((ST_TICKS - ${tprev[$tkey]:-0}))
          if [ "$dd" -gt "$top_d" ]; then top_d=$dd; top_name=${tname[$tkey]}; fi
        fi
        tprev[$tkey]=$ST_TICKS
      done
      for tkey in "${!tprev[@]}"; do
        [ -n "${tseen[$tkey]:-}" ] || unset 'tprev[$tkey]' 'tname[$tkey]'
      done
      unset tseen
    fi
    if [ "$have_prev" -eq 1 ]; then
      dt_ms=$((NOW_MS - prev_ms))
      [ "$dt_ms" -gt 0 ] || dt_ms=1
      if [ $((total - prev_total)) -gt 0 ]; then
        mt=$(((busy - prev_busy) * ncpu * 1000 / (total - prev_total)))
      else
        mt=0
      fi
      dec "$mt"
      line="{\"t_ms\":$NOW_MS,\"dt_ms\":$dt_ms,\"ncpu\":$ncpu,\"machine\":$DEC"
      local tree=$((sum[compositor] + sum[renderer] + sum[gpu] + sum[utility] + sum[zygote] + sum[cef_other] + sum[xvfb]))
      mcores "$tree" "$dt_ms"
      dec "$MC"
      line+=",\"tree\":$DEC,\"cats\":{"
      local first=1
      for c in "${cats[@]}"; do
        mcores "${sum[$c]}" "$dt_ms"
        dec "$MC"
        [ "$first" -eq 1 ] || line+=","
        first=0
        line+="\"$c\":$DEC"
      done
      line+="},\"renderers\":[$renderers]"
      if [ "$top_d" -ge 0 ]; then
        mcores "$top_d" "$dt_ms"
        dec "$MC"
        line+=",\"top_thread\":{\"name\":\"$top_name\",\"cores\":$DEC}"
      else
        line+=",\"top_thread\":null"
      fi
      line+=",\"mem_used_mb\":$(((mem_total - mem_avail) / 1024)),\"procs\":$procs}"
      printf '%s\n' "$line"
    fi
    unset seen
    prev_busy=$busy
    prev_total=$total
    prev_ms=$NOW_MS
    have_prev=1
    k=$((k + 1))
    [ $((NOW_MS - t0)) -lt $((max_s * 1000)) ] || break
    if [ -z "${COST_PROC_SEQ:-}" ]; then
      # The next whole second after the first read, so a slow read never drifts the cadence.
      clock
      local wait_ms=$((1000 - (NOW_MS - t0) % 1000))
      printf -v dd '%d.%03d' $((wait_ms / 1000)) $((wait_ms % 1000))
      read -r -t "$dd" -u "$sleep_fd" _ || true
    fi
  done
}

cmd_kill_renderer() {
  local d p victims=() clients=() rc=0 out=""
  for d in "$PROC"/[0-9]*; do
    p=${d##*/}
    type_of "$p"
    if [ "$TYPE" = renderer ]; then
      victims+=("$p")
      clients+=("${CLIENT:-null}")
    fi
  done
  clock
  local IFS=,
  printf '{"at_ms":%s,"killed":[%s],"clients":[%s]}\n' "$NOW_MS" "${victims[*]}" "${clients[*]}"
  unset IFS
  if [ "${#victims[@]}" -eq 0 ]; then
    clock
    printf '{"rc_at_ms":%s,"rc":1}\n' "$NOW_MS"
    exit 1
  fi
  kill -KILL "${victims[@]}" || rc=$?
  clock
  printf '{"rc_at_ms":%s,"rc":%s}\n' "$NOW_MS" "$rc"
  [ "$rc" -eq 0 ]
}

case "${1:-}" in
  sample) cmd_sample "${2:-}" ;;
  kill-renderer)
    [ -z "${COST_PROC_SEQ:-}" ] || PROC="$COST_PROC_SEQ/0"
    cmd_kill_renderer
    ;;
  *)
    echo '{"error":"usage: sample <max_s> | kill-renderer"}'
    exit 64
    ;;
esac
