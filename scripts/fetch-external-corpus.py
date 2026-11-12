#!/usr/bin/env python3
"""Fetch the pinned VS Code zh-Hans / zh-Hant localization pairs.

Output: <out>/pairs.json, a list of {id, src, ref, opencc} where src is the
Simplified string, ref is Microsoft's published zh-Hant string for the same
key, and opencc is OpenCC s2twp applied to src. tests/external-corpus.rs reads
it and scores zhtw-mcp convert against the same references.

The upstream commit and the sha256 of every fetched file are pinned in
tests/fixtures/external-corpus-pins.json. A fetched file whose hash differs
from the pin is an error, so the numbers cannot drift because upstream moved.
The opencc baseline comes from the installed opencc package, which is not
pinned here, so its column can move with that package; the pinned part is the
source and reference text.
Published translations are evidence, not ground truth.

Usage:
    python3 scripts/fetch-external-corpus.py [--out DIR]
    python3 scripts/fetch-external-corpus.py --record   # (re)write the pins

Needs network and the Python package "opencc" (pip install opencc).
"""

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

REPO = "microsoft/vscode-loc"
# Commit of the repository's main branch on 2026-10-19.
COMMIT = "62d5443b567022539ba6e8565b0f43f5f6b3682b"
BASE = "i18n/vscode-language-pack-zh-{}/translations"
ROOT = Path(__file__).resolve().parent.parent
PINS = ROOT / "tests" / "fixtures" / "external-corpus-pins.json"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "zhtw-mcp-bench"})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.read()


def raw_url(path):
    return f"https://raw.githubusercontent.com/{REPO}/{COMMIT}/{path}"


def list_files():
    """Translation bundle paths relative to the zh-Hans translations dir."""
    base = BASE.format("hans")
    names = ["main.i18n.json"]
    api = f"https://api.github.com/repos/{REPO}/contents/{base}/extensions?ref={COMMIT}"
    for entry in json.loads(get(api)):
        if entry["name"].endswith(".i18n.json"):
            names.append("extensions/" + entry["name"])
    return sorted(names)


def fetch_bundle(rel, pins, record):
    out = {}
    for lang in ("hans", "hant"):
        path = f"{BASE.format(lang)}/{rel}"
        try:
            data = get(raw_url(path))
        except Exception as exc:
            # Recording tolerates a bundle that exists for one language only.
            # A pinned run was recorded complete, so a missing bundle means a
            # network fault, and a partial pairs file would be scored and cached
            # as the benchmark.
            if not record:
                sys.exit(f"fetch failed for pinned file {path}: {exc}")
            print(f"skip {path}: {exc}", file=sys.stderr)
            return None
        digest = hashlib.sha256(data).hexdigest()
        if record:
            pins["files"][path] = digest
        elif pins["files"].get(path) != digest:
            sys.exit(f"hash mismatch for {path}: {digest}")
        out[lang] = json.loads(data)["contents"]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "target" / "external-corpus"))
    ap.add_argument("--record", action="store_true")
    args = ap.parse_args()

    import opencc

    s2twp = opencc.OpenCC("s2twp")

    if args.record:
        pins = {"repo": REPO, "commit": COMMIT, "files": {}}
        rels = list_files()
        pins["bundles"] = rels
    else:
        pins = json.loads(PINS.read_text())
        if pins["commit"] != COMMIT:
            sys.exit("pins file and script disagree on the commit")
        rels = pins["bundles"]

    pairs = []
    for rel in rels:
        bundle = fetch_bundle(rel, pins, args.record)
        if bundle is None:
            continue
        for section, hans_msgs in bundle["hans"].items():
            hant_msgs = bundle["hant"].get(section, {})
            for key, src in hans_msgs.items():
                ref = hant_msgs.get(key)
                if not isinstance(src, str) or not isinstance(ref, str):
                    continue
                # One line per string keeps the Rust side line-oriented, and a
                # carriage return would be eaten by line splitting. A string
                # with no Han exercises nothing.
                # A tab would shift the columns of the tab-separated report.
                if any(c in s for s in (src, ref) for c in "\n\r\t") or not any(
                    "一" <= c <= "鿿" for c in src
                ):
                    continue
                pairs.append(
                    {
                        "id": f"{rel}:{section}:{key}",
                        "src": src,
                        "ref": ref,
                        "opencc": s2twp.convert(src),
                    }
                )

    if args.record:
        PINS.write_text(
            json.dumps(pins, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(f"recorded {len(pins['files'])} files at {COMMIT}")

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    # Written to a temporary name and renamed, so an interrupted run cannot
    # leave a truncated file for the test to reuse.
    tmp = out / "pairs.json.tmp"
    tmp.write_text(json.dumps(pairs, ensure_ascii=False), encoding="utf-8")
    tmp.replace(out / "pairs.json")
    print(f"wrote {len(pairs)} pairs to {out / 'pairs.json'}")


if __name__ == "__main__":
    main()
