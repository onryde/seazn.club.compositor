"""In-process scrubber (C00 spike, THROWAWAY). Same rules as redact.py, applied to strings
before they enter a RESULT json, so the line-dropping stage of redact.py never has to drop it."""
import os
import re
import urllib.parse

_parts = set()
for _k, _v in os.environ.items():
    if _k.startswith("SPIKE_") and _v:
        for _c in (_v, urllib.parse.unquote(_v)):
            _parts.add(_c)
            for _m in re.finditer(r"[?&]([^=&\s]+)=([^&\s]+)", _c):
                _parts.add(_m.group(2))
                _parts.add(urllib.parse.unquote(_m.group(2)))
            _m = re.search(r"/live2?/([^?\s]+)", _c)
            if _m:
                _parts.add(_m.group(1))
                _parts.update(_m.group(1).split("/"))
_parts = sorted((p for p in _parts if len(p) >= 8), key=len, reverse=True)


def scrub(s):
    if not isinstance(s, str):
        return s
    for p in _parts:
        s = s.replace(p, "<redacted>")
    s = re.sub(r"\b(srt|rtmps?)://[^\s\"'<>]+", lambda m: m.group(1) + "://<redacted>", s, flags=re.I)
    s = re.sub(r"/live2?/[^\s\"'<>]+", "/live/<redacted>", s)
    s = re.sub(r"\?[^\s\"'<>]+", "?<q>", s)
    s = re.sub(r"passphrase|streamid", "<cred-field>", s, flags=re.I)
    return s
