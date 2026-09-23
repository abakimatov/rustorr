#!/usr/bin/env python3
"""Cache key for reference corpora.

The pinned reference does not change between runs, so a valid corpus can be
reused while everything that shapes it stays the same: the runner and its
manifest, the fixture sources, the resolved compose configuration of the
reference side and the IDs of its images, the torrent fixture and the capture
arguments. Candidate services are left out of the key: rebuilding Rustorr must
not invalidate the reference.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
from typing import Any

SCHEMA = "rustorr.reference-cache.v1"
CANDIDATE_SERVICES = {"rustorr", "r6proxy"}
INPUTS = (
    "tools/contract/run.py",
    "tools/contract/scenarios.json",
    "tools/contract/accs.db",
    "tools/contract/docker",
    "tools/contract/r7",
    "tools/baseline/generate-fixtures.py",
    "tools/baseline/fixture_payload.py",
    "tools/baseline/docker",
)


def file_digests(root: pathlib.Path) -> dict[str, str]:
    digests = {}
    for entry in INPUTS:
        path = root / entry
        files = sorted(path.rglob("*")) if path.is_dir() else [path]
        for file in files:
            if file.is_file() and "__pycache__" not in file.parts:
                digests[str(file.relative_to(root))] = hashlib.sha256(file.read_bytes()).hexdigest()
    return digests


def image_id(image: str) -> str:
    result = subprocess.run(
        ["docker", "image", "inspect", "--format", "{{.Id}}", image],
        capture_output=True,
        text=True,
        check=False,
    )
    # A missing image makes the key unique to this state; the capture builds
    # it and the corpus is stored under the key computed afterwards.
    return result.stdout.strip() if result.returncode == 0 else f"missing:{image}"


def reference_side(compose: dict[str, Any]) -> tuple[dict[str, Any], dict[str, str]]:
    services = {name: spec for name, spec in compose["services"].items() if name not in CANDIDATE_SERVICES}
    images = {}
    for name, spec in sorted(services.items()):
        image = spec.get("image") or f"{compose['name']}-{name}"
        images[name] = image_id(image)
    return services, images


def describe(args: argparse.Namespace) -> dict[str, Any]:
    compose = json.loads(sys.stdin.read())
    services, images = reference_side(compose)
    return {
        "schema": SCHEMA,
        "capture": args.capture,
        "base_url": args.base_url,
        "reset_seeder": args.reset_seeder,
        "torrent_file": hashlib.sha256(args.torrent_file.read_bytes()).hexdigest(),
        "inputs": file_digests(args.root),
        "compose": hashlib.sha256(json.dumps(services, sort_keys=True).encode()).hexdigest(),
        "images": images,
    }


def key_of(description: dict[str, Any]) -> str:
    return hashlib.sha256(json.dumps(description, sort_keys=True).encode()).hexdigest()


def command_key(args: argparse.Namespace) -> None:
    description = describe(args)
    key = key_of(description)
    if args.describe:
        args.describe.write_text(json.dumps(description, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(key)


def command_store(args: argparse.Namespace) -> None:
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    if not corpus.get("valid"):
        print(f"reference cache: {args.corpus} is not valid, not stored", file=sys.stderr)
        return
    description = json.loads(args.describe.read_text(encoding="utf-8"))
    key = key_of(description)
    args.cache_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(args.corpus, args.cache_dir / f"{key}.json")
    shutil.copyfile(args.describe, args.cache_dir / f"{key}.key.json")
    print(f"reference cache: stored {key}", file=sys.stderr)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    key = commands.add_parser("key", help="print the key; compose config JSON comes on stdin")
    key.add_argument("--root", type=pathlib.Path, required=True)
    key.add_argument("--base-url", required=True)
    key.add_argument("--reset-seeder", required=True)
    key.add_argument("--torrent-file", type=pathlib.Path, required=True)
    key.add_argument("--describe", type=pathlib.Path, help="write the key inputs here")
    key.add_argument("capture", nargs=argparse.REMAINDER, help="capture arguments after --")
    key.set_defaults(handler=command_key)

    store = commands.add_parser("store", help="store a valid corpus under the key of its description")
    store.add_argument("--cache-dir", type=pathlib.Path, required=True)
    store.add_argument("--describe", type=pathlib.Path, required=True)
    store.add_argument("corpus", type=pathlib.Path)
    store.set_defaults(handler=command_store)

    args = parser.parse_args()
    if args.command == "key" and args.capture[:1] == ["--"]:
        args.capture = args.capture[1:]
    args.handler(args)


if __name__ == "__main__":
    main()
