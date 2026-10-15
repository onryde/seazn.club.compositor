#!/bin/sh
# GST_CEF_SUBPROCESS_PATH wrapper: every CEF child starts at nice 10 with oom_score_adj 1000 (C11 guardrails).
echo 1000 > /proc/self/oom_score_adj 2>/dev/null
exec nice -n 10 "$CEF_SUBPROCESS_PATH" "$@"
