#!/usr/bin/env python3
"""Check license harvesting limitations and keep the release inventory current.

Requires cargo-about 0.9.2. No permissive SPDX fallback may appear silently:
known missing upstream texts are version-scoped in license-fallbacks.json.
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def check_fallbacks(inventory, reviews):
    """Reject new, changed, duplicated, or obsolete canonical-text exceptions."""
    expected = {}
    for entry in reviews:
        key = (entry["name"], entry["version"], entry["license"])
        if key in expected or not entry.get("reason") or not entry.get("upstream"):
            raise ValueError(f"Incomplete or duplicate fallback review: {key}")
        expected[key] = entry
    actual = set()
    for license_info in inventory["licenses"]:
        if license_info.get("source_path"):
            continue
        for usage in license_info["used_by"]:
            crate = usage["crate"]
            actual.add((crate["name"], crate["version"], license_info["id"]))
    if actual != set(expected):
        raise ValueError(
            f"License-text review required. Unreviewed: {sorted(actual - expected.keys())}; "
            f"obsolete reviews: {sorted(expected.keys() - actual)}"
        )


def cargo_about(*args):
    return subprocess.run(
        ["cargo", "about", *args], cwd=ROOT, check=True,
        stdout=subprocess.PIPE, encoding="utf-8",
    ).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="regenerate the checked-in inventory")
    args = parser.parse_args()
    version = cargo_about("--version").strip()
    if version != "cargo-about 0.9.2":
        raise ValueError(f"Expected cargo-about 0.9.2, found {version}")
    options = ["generate", "--locked", "--all-features", "--fail"]
    inventory = json.loads(cargo_about(*options, "--format", "json"))
    reviews = json.loads((ROOT / "packaging/license-fallbacks.json").read_text(encoding="utf-8"))
    check_fallbacks(inventory, reviews)
    rendered = cargo_about(*options, "packaging/licenses.hbs")
    # Normalize layout only; retain the words and attribution in upstream texts.
    rendered = "\n".join(line.rstrip(" \t") for line in rendered.split("\n")).rstrip() + "\n"
    output = ROOT / "THIRD_PARTY_LICENSES.md"
    if args.write:
        output.write_bytes(rendered.encode("utf-8"))
    elif not output.exists() or output.read_text(encoding="utf-8") != rendered:
        raise ValueError("THIRD_PARTY_LICENSES.md is stale; run python3 scripts/check-licenses.py --write")
    print(f"License inventory verified: {len(inventory['crates'])} packages; "
          f"{len(reviews)} explicitly documented upstream-text limitations.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
