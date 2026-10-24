"""DO NOT MERGE. Owed 87's adapter mutants: `python3 -I mutants/owed87.py <id> apply|field <name>`."""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
m = json.loads((ROOT / "mutants/owed87.json").read_text())["mutants"][sys.argv[1]]
if sys.argv[2] == "field":
    print(m[sys.argv[3]])
elif sys.argv[2] == "apply":
    p = ROOT / m["file"]
    s = p.read_text()
    n = s.count(m["old"])
    if n != 1:
        sys.exit(f"{sys.argv[1]}: its text occurs {n} times in {m['file']}")
    p.write_text(s.replace(m["old"], m["new"]))
    marker = m["new"][m["new"].index("/*MUT") :]
    marker = marker[: marker.index("*/") + 2]
    if marker not in p.read_text():
        sys.exit(f"{sys.argv[1]}: no {marker} after the replace")
    print(f"{sys.argv[1]} applied to {m['file']}")
else:
    sys.exit("usage: owed87.py <id> apply|field <name>")
