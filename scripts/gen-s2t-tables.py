#!/usr/bin/env python3
"""Generate Rust source from OpenCC dictionary text files.

Downloads STCharacters.txt, STPhrases.txt, and TWVariants.txt from the
OpenCC GitHub repository and generates a Rust source file with static
arrays that can be compiled into the binary.

This eliminates include_str! + runtime text parsing: the dictionaries
are pre-parsed into typed Rust arrays at code-generation time.

Usage:
  python3 scripts/gen-s2t-tables.py                  # download + generate
  python3 scripts/gen-s2t-tables.py --dry-run         # print stats only
  python3 scripts/gen-s2t-tables.py --check           # verify generated file is up-to-date
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUTPUT = REPO / "src" / "engine" / "s2t_data.rs"

# Pinned OpenCC commit.  This used to say "master", with a comment claiming
# that was for reproducibility, which it is not: master moves, downloaded
# files are cached forever, so two developers at the same repo commit built
# different conversion tables depending on when they first cloned.
#
# To bump: change OPENCC_COMMIT, run this script, and paste the printed
# source hash into EXPECTED_SOURCE_HASH.  The check below refuses to
# generate if only one of the two moved, so a bump cannot be half-applied.
#
# Re-pin flake.nix and flake.lock to the same commit, because the Nix build
# fills this cache from that input instead of downloading:
#   nix flake lock --override-input opencc-src github:BYVoid/OpenCC/<commit>
OPENCC_COMMIT = "5249273a3e5606852f088c9a8b23522145d94f78"
EXPECTED_SOURCE_HASH = "959b88f60c9dcc0d"

OPENCC_RAW = (
    f"https://raw.githubusercontent.com/BYVoid/OpenCC/{OPENCC_COMMIT}/data/dictionary"
)

# Cache keyed by commit, so bumping the pin fetches fresh files instead of
# silently reusing whatever an earlier run happened to download.
DICT_DIR = REPO / "data" / "opencc" / OPENCC_COMMIT[:12]

# Dictionary files to process.
DICT_FILES = {
    "st_phrases": ("STPhrases.txt", f"{OPENCC_RAW}/STPhrases.txt"),
    "st_characters": ("STCharacters.txt", f"{OPENCC_RAW}/STCharacters.txt"),
    "tw_variants": ("TWVariants.txt", f"{OPENCC_RAW}/TWVariants.txt"),
}


def ensure_downloaded() -> None:
    """Download dictionary files from GitHub if not already cached."""
    DICT_DIR.mkdir(parents=True, exist_ok=True)
    for name, (filename, url) in DICT_FILES.items():
        path = DICT_DIR / filename
        if path.exists():
            continue
        print(f"Downloading {filename} ...", file=sys.stderr)
        try:
            urllib.request.urlretrieve(url, path)
        except Exception as e:
            print(f"error: failed to download {url}: {e}", file=sys.stderr)
            sys.exit(1)


def dict_path(name: str) -> Path:
    """Return local path for a dictionary file."""
    filename, _ = DICT_FILES[name]
    return DICT_DIR / filename


# Genuinely two-way chars curated by hand: each has two common TC readings
# (e.g. 后 後/后, 里 裡/里) so single-char fallback would guess wrong. This is a
# deliberate subset, not "every char OpenCC lists with >1 candidate" -- most of
# those (个 個, 万 萬, 当 當) have an overwhelmingly dominant reading and are safe.
#
# Excluding a char means it falls back to identity, which for a Simplified-only
# char such as 复 leaves Simplified in the output. That residual is deliberate:
# guessing one reading instead mis-converts the others (see EXTRA_PHRASES).
# Reduce it by adding phrase entries, never by picking a default character.
MANUAL_AMBIGUOUS_CHARS = {
    "干",
    "复",
    "咸",
    "范",
    "丑",
    "佣",
    "伙",
    "舍",
    "症",
    "姜",
    "沈",
    "克",
    "后",
    "里",
    "余",
}


def parse_dict(path: Path, *, keep_identity: bool = False) -> list[tuple[str, str]]:
    """Parse a tab-separated OpenCC dictionary file.

    Returns (key, primary_value) pairs. Skips comments, empty lines,
    and, unless requested, identity mappings. For one-to-many mappings, takes
    the first value.
    """
    entries = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t", 1)
            if len(parts) != 2:
                continue
            key = parts[0]
            val = parts[1].split()[0]  # first space-separated value
            if key == val and not keep_identity:
                continue  # identity mapping
            entries.append((key, val))
    # Preserve pre-sorted deterministic ordering (longest key first, then lexicographical)
    entries.sort(key=lambda x: (-len(x[0]), x[0]))
    return entries


def parse_char_dict(path: Path) -> list[tuple[str, str]]:
    """Parse a character dictionary, keeping only single-char -> single-char entries."""
    entries = []
    for key, val in parse_dict(path):
        if len(key) == 1 and len(val) == 1:
            entries.append((key, val))
    # Preserve pre-sorted deterministic ordering (longest key first, then lexicographical)
    entries.sort(key=lambda x: (-len(x[0]), x[0]))
    return entries


def parse_ambiguous_chars() -> set[str]:
    """Return chars that must never use unconditional single-char fallback."""
    return set(MANUAL_AMBIGUOUS_CHARS)


# Phrase entries OpenCC's STPhrases lacks, added so an ambiguous char resolves
# from context instead of falling back to identity.  Each pair is hand-verified
# zh-TW; do not guess a reading here.  This is the only safe way to reduce the
# residual: picking one reading for the bare character mis-converts the others
# (复 alone is 復/複/覆, and in technical prose 複 dominates).
#
# Purely additive: upstream always wins.  DICT_FILES tracks OpenCC master, so an
# entry that is a gap today can be filled upstream tomorrow, and that must not
# break the build.  Redundant and overridden entries are reported so they can be
# retired, not treated as errors.
EXTRA_PHRASES = {
    "复盘": "復盤",
    "复联": "復聯",
}


def filter_safe_chars(
    chars: list[tuple[str, str]], ambiguous_chars: set[str]
) -> list[tuple[str, str]]:
    """Remove ambiguous chars from the single-char fallback table."""
    return [(key, val) for key, val in chars if key not in ambiguous_chars]


def parse_phrases_with_ambiguous_protection(
    path: Path, ambiguous_chars: set[str]
) -> tuple[list[tuple[str, str]], int]:
    """Parse STPhrases and keep identity rows only when they protect ambiguous chars.

    Returns the entries and how many EXTRA_PHRASES supplements were actually
    applied, which is fewer than len(EXTRA_PHRASES) once upstream catches up.
    """
    entries = []
    applied = 0
    seen = {}
    for key, val in parse_dict(path, keep_identity=True):
        if key == val and not any(ch in ambiguous_chars for ch in key):
            continue
        if key in seen:
            continue
        seen[key] = val
        entries.append((key, val))
    for key, val in EXTRA_PHRASES.items():
        if key in seen:
            upstream = seen[key]
            note = "same value" if upstream == val else f"upstream keeps {upstream!r}"
            print(
                f"note: EXTRA_PHRASES[{key!r}] is now covered by STPhrases "
                f"({note}); the local entry is unused and can be retired.",
                file=sys.stderr,
            )
            continue
        entries.append((key, val))
        applied += 1
    # Already ordered: parse_dict() sorts, and the filtering above preserves it.
    # EXTRA_PHRASES lands at the end, which is fine, because the AC is
    # leftmost-longest and every key is unique, so pattern order cannot affect
    # matching.
    return entries, applied


def compute_source_hash() -> str:
    """SHA-256 of all source dictionary files (sorted by name)."""
    h = hashlib.sha256()
    for name in sorted(DICT_FILES):
        path = dict_path(name)
        h.update(path.read_bytes())
    return h.hexdigest()[:16]


def escape_rust_str(s: str) -> str:
    """Escape a string for use in a Rust string literal."""
    return s.replace("\\", "\\\\").replace('"', '\\"')


def generate_rust(
    phrases: list[tuple[str, str]],
    extras_applied: int,
    chars: list[tuple[str, str]],
    ambiguous_chars: set[str],
    tw_variants: list[tuple[str, str]],
    source_hash: str,
) -> str:
    """Generate s2t_data.rs content."""
    lines = [
        "// Auto-generated by scripts/gen-s2t-tables.py — do not edit manually.",
        f"// Source hash: {source_hash}",
        "// Dictionary data: Apache-2.0 (OpenCC project).",
        "",
        "/// SC->TC phrase mappings (longest-match substitution).",
        f"/// {len(phrases)} entries: STPhrases.txt plus "
        f"{extras_applied} local additions (EXTRA_PHRASES).",
        f"pub const ST_PHRASES: &[(&str, &str)] = &[",
    ]

    for key, val in phrases:
        lines.append(f'    ("{escape_rust_str(key)}", "{escape_rust_str(val)}"),')

    lines.append("];")
    lines.append("")

    lines.append(
        "/// SC->TC safe single-character mappings (fallback after phrase matching)."
    )
    lines.append(
        "/// Ambiguous one-to-many chars are deliberately excluded and fall back to identity."
    )
    lines.append(
        f"/// {len(chars)} entries from STCharacters.txt after ambiguity filtering."
    )
    lines.append(f"pub const ST_CHARACTERS: &[(char, char)] = &[")

    for key, val in chars:
        lines.append(f"    ('{escape_rust_str(key)}', '{escape_rust_str(val)}'),")

    lines.append("];")
    lines.append("")

    lines.append("/// Ambiguous SC chars excluded from ST_CHARACTERS.")
    lines.append(f"/// {len(ambiguous_chars)} entries.")
    lines.append("#[rustfmt::skip]")
    lines.append("pub const AMBIGUOUS_ST_CHARACTERS: &[char] = &[")

    for ch in sorted(ambiguous_chars):
        lines.append(f"    '{escape_rust_str(ch)}',")

    lines.append("];")
    lines.append("")

    lines.append("/// Taiwan variant normalization (applied last).")
    lines.append(f"/// {len(tw_variants)} entries from TWVariants.txt.")
    lines.append(f"pub const TW_VARIANTS: &[(char, char)] = &[")

    for key, val in tw_variants:
        lines.append(f"    ('{escape_rust_str(key)}', '{escape_rust_str(val)}'),")

    lines.append("];")
    lines.append("")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="Print stats only")
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify generated file is up-to-date",
    )
    args = parser.parse_args()

    # Download dictionaries if needed.
    ensure_downloaded()

    # Verify dictionary files exist.
    for name in DICT_FILES:
        path = dict_path(name)
        if not path.exists():
            print(f"error: {name} not found at {path}", file=sys.stderr)
            sys.exit(1)

    ambiguous_chars = parse_ambiguous_chars()

    phrases, extras_applied = parse_phrases_with_ambiguous_protection(
        dict_path("st_phrases"), ambiguous_chars
    )
    chars_raw = parse_char_dict(dict_path("st_characters"))
    chars = filter_safe_chars(chars_raw, ambiguous_chars)
    tw_variants = parse_char_dict(dict_path("tw_variants"))
    source_hash = compute_source_hash()

    # The pin is only worth having if something checks it.  A mismatch means
    # either the pinned commit was bumped without refreshing the expected
    # hash, or the cached files no longer match the commit they are filed
    # under; both produce conversion tables nobody reviewed.
    if source_hash != EXPECTED_SOURCE_HASH:
        print(
            f"error: dictionary source hash {source_hash} does not match "
            f"EXPECTED_SOURCE_HASH {EXPECTED_SOURCE_HASH}.\n"
            f"  If you bumped OPENCC_COMMIT (currently {OPENCC_COMMIT[:12]}), "
            f'review the diff and set EXPECTED_SOURCE_HASH = "{source_hash}".\n'
            f"  Otherwise the cache at {DICT_DIR} is corrupt: delete it and re-run.",
            file=sys.stderr,
        )
        sys.exit(1)

    safe_keys = {key for key, _ in chars}
    overlap = safe_keys & ambiguous_chars
    if overlap:
        print(
            f"error: ambiguous chars leaked into ST_CHARACTERS: {sorted(overlap)}",
            file=sys.stderr,
        )
        sys.exit(1)

    print(
        f"STPhrases:    {len(phrases):>6} entries "
        f"({extras_applied} of {len(EXTRA_PHRASES)} local applied)"
    )
    print(f"STCharacters: {len(chars):>6} entries (safe single-char only)")
    print(f"Ambiguous:    {len(ambiguous_chars):>6} chars excluded")
    print(f"TWVariants:   {len(tw_variants):>6} entries")
    print(f"Source hash:  {source_hash}")

    if args.dry_run:
        return

    content = generate_rust(
        phrases, extras_applied, chars, ambiguous_chars, tw_variants, source_hash
    )

    if args.check:
        if not OUTPUT.exists():
            print(f"error: {OUTPUT} does not exist", file=sys.stderr)
            sys.exit(1)
        existing = OUTPUT.read_text(encoding="utf-8")
        if existing == content:
            print(f"OK: {OUTPUT.name} is up-to-date")
        else:
            print(
                f"error: {OUTPUT.name} is stale — run: python3 scripts/gen-s2t-tables.py",
                file=sys.stderr,
            )
            sys.exit(1)
        return

    OUTPUT.write_text(content, encoding="utf-8")
    print(f"Wrote {OUTPUT} ({len(content)} bytes)")


if __name__ == "__main__":
    main()
