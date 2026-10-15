#!/usr/bin/env bash
# usage: src-switch.sh <rtmps|srt> : stop any running source loop, start the given one
pkill -f "[r]un-source" ; pkill -f "[s]ource_feed" ; sleep 2
setsid nohup /spike/run-source.sh "$1" 3000 > /dev/null 2>&1 < /dev/null &
sleep 12
tail -3 /tmp/c00/source-$1.log | cut -c1-200
