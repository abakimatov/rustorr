#!/usr/bin/env python3
"""Create deterministic baseline data and BitTorrent metadata."""

from __future__ import annotations

import hashlib
import json
import pathlib
import shutil
import struct
import sys
from typing import Any

TRACKER = b"http://tracker:6969/announce"
PIECE_LENGTH = 256 * 1024
SEED = b"rustorr-r1-fixture-v1"


def bencode(value: Any) -> bytes:
    if isinstance(value, int):
        return b"i" + str(value).encode() + b"e"
    if isinstance(value, bytes):
        return str(len(value)).encode() + b":" + value
    if isinstance(value, str):
        return bencode(value.encode())
    if isinstance(value, list):
        return b"l" + b"".join(bencode(item) for item in value) + b"e"
    if isinstance(value, dict):
        items = sorted(value.items(), key=lambda item: item[0] if isinstance(item[0], bytes) else item[0].encode())
        return b"d" + b"".join(bencode(key) + bencode(item) for key, item in items) + b"e"
    raise TypeError(type(value).__name__)


def payload(size: int, offset: int = 0) -> bytes:
    output = bytearray()
    counter = offset // len(SEED)
    while len(output) < size:
        output.extend(hashlib.sha256(SEED + struct.pack(">Q", counter)).digest())
        counter += 1
    return bytes(output[:size])


def write_file(path: pathlib.Path, size: int) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    with path.open("wb") as stream:
        remaining = size
        offset = 0
        while remaining:
            chunk = min(1024 * 1024, remaining)
            data = payload(chunk, offset)
            stream.write(data)
            digest.update(data)
            remaining -= chunk
            offset += chunk
    return digest.hexdigest()


def torrent(name: str, files: list[tuple[str, int]], root: pathlib.Path, disk_prefix: str = "") -> tuple[bytes, str]:
    pieces = bytearray()
    pending = bytearray()
    for relative, _size in files:
        with (root / disk_prefix / relative).open("rb") as stream:
            while True:
                block = stream.read(PIECE_LENGTH - len(pending))
                if not block:
                    break
                pending.extend(block)
                if len(pending) == PIECE_LENGTH:
                    pieces.extend(hashlib.sha1(pending).digest())
                    pending.clear()
    if pending:
        pieces.extend(hashlib.sha1(pending).digest())

    if len(files) == 1 and files[0][0] == name:
        info: dict[bytes, Any] = {b"length": files[0][1], b"name": name.encode()}
    else:
        info = {
            b"files": [
                {b"length": size, b"path": [part.encode() for part in relative.split("/")]}
                for relative, size in files
            ],
            b"name": name.encode(),
        }
    info.update({b"piece length": PIECE_LENGTH, b"pieces": bytes(pieces)})
    encoded_info = bencode(info)
    return bencode({b"announce": TRACKER, b"info": info}), hashlib.sha1(encoded_info).hexdigest()


def main(destination: str) -> None:
    root = pathlib.Path(destination)
    files_root = root / "files"
    torrents_root = root / "torrents"
    shutil.rmtree(files_root, ignore_errors=True)
    shutil.rmtree(torrents_root, ignore_errors=True)
    files_root.mkdir(parents=True, exist_ok=True)
    torrents_root.mkdir(parents=True, exist_ok=True)

    definitions = {
        "single": {"name": "single.bin", "files": [("single.bin", 8 * 1024 * 1024)]},
        "multi": {"name": "multi", "disk_prefix": "multi", "files": [("part-a.bin", 3 * 1024 * 1024), ("part-b.bin", 5 * 1024 * 1024)]},
        "unicode": {
            "name": "Медиа коллекция",
            "disk_prefix": "unicode",
            "files": [("01 Пример/Фильм.mkv", 512 * 1024), ("02 Пример/Фильм.srt", 32 * 1024), ("02 Пример/Фильм.ac3", 64 * 1024)],
        },
    }
    manifest = {"piece_length": PIECE_LENGTH, "seed": SEED.decode(), "torrents": {}}
    torrent_lines = []
    info_hashes = []
    for name, definition in definitions.items():
        files = definition["files"]
        disk_prefix = definition.get("disk_prefix", "")
        hashes = {
            str(pathlib.Path(disk_prefix) / relative): write_file(files_root / disk_prefix / relative, size)
            for relative, size in files
        }
        metadata, info_hash = torrent(definition["name"], files, files_root, disk_prefix)
        torrent_path = torrents_root / f"{name}.torrent"
        torrent_path.write_bytes(metadata)
        torrent_lines.append(f"/data/torrents/{name}.torrent")
        info_hashes.append(info_hash)
        manifest["torrents"][name] = {
            "files": files,
            "sha256": hashes,
            "torrent_sha256": hashlib.sha256(metadata).hexdigest(),
            "info_hash": info_hash,
        }
    (torrents_root / "all.txt").write_text("\n".join(torrent_lines) + "\n", encoding="utf-8")
    (root / "whitelist.txt").write_text("\n".join(info_hashes) + "\n", encoding="utf-8")
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: generate-fixtures.py DESTINATION")
    main(sys.argv[1])
