#!/usr/bin/env python3
"""Deterministic R7 fixtures shared by the reference and the candidate.

``rutor.ls`` has the format MatriX.145 downloads from
``releases.yourok.ru``: a raw DEFLATE stream holding one JSON array of
torrent records. ``TORZNAB_ITEMS`` feeds the fake Torznab indexer.
"""

from __future__ import annotations

import json
import pathlib
import sys
import zlib

MEDIA_HASH = "d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d"
SINGLE_HASH = "68c3ccdd52b2925f4f97e2f61ea248e728e54cea"

RUTOR = [
    {
        "Title": "Фильм / Film (2021) WEB-DL 1080p",
        "Name": "Фильм",
        "Names": ["Фильм", "Film"],
        "Categories": "Movie",
        "Size": "4.37 GB",
        "CreateDate": "2021-05-01T12:00:00Z",
        "Tracker": "Rutor",
        "Link": "http://rutor.invalid/torrent/1",
        "Year": 2021,
        "Peer": 3,
        "Seed": 42,
        "Magnet": f"magnet:?xt=urn:btih:{MEDIA_HASH}",
        "Hash": MEDIA_HASH,
        "IMDBID": "tt0000001",
        "VideoQuality": 200,
        "AudioQuality": 203,
    },
    {
        "Title": "Сериал / Series (2020) S01 BDRip 720p",
        "Name": "Сериал",
        "Names": ["Сериал", "Series"],
        "Categories": "Series",
        "Size": "12.1 GB",
        "CreateDate": "2020-11-20T08:30:00Z",
        "Tracker": "Rutor",
        "Link": "http://rutor.invalid/torrent/2",
        "Year": 2020,
        "Peer": 1,
        "Seed": 7,
        "Magnet": f"magnet:?xt=urn:btih:{SINGLE_HASH}",
        "Hash": SINGLE_HASH,
        "IMDBID": "tt0000002",
        "VideoQuality": 101,
        "AudioQuality": 201,
    },
    {
        "Title": "Мультфильм / Cartoon (2019) BDRemux 1080p",
        "Name": "Мультфильм",
        "Names": ["Мультфильм", "Cartoon"],
        "Categories": "CartoonMovie",
        "Size": "20.5 GB",
        "CreateDate": "2019-01-02T00:00:00Z",
        "Tracker": "Rutor",
        "Link": "http://rutor.invalid/torrent/3",
        "Year": 2019,
        "Peer": 0,
        "Seed": 1,
        "Magnet": "magnet:?xt=urn:btih:1178d2eb561f45ca35859be366af777b92111cbc",
        "Hash": "1178d2eb561f45ca35859be366af777b92111cbc",
        "IMDBID": "",
        "VideoQuality": 203,
        "AudioQuality": 300,
    },
]

TORZNAB_ITEMS = [
    {
        "title": "Film 2021 1080p WEB-DL",
        "link": "http://indexer.invalid/download/1",
        "pubDate": "Sat, 01 May 2021 12:00:00 +0000",
        "size": 4692251852,
        "indexer": "FakeIndexer",
        "magnet": f"magnet:?xt=urn:btih:{MEDIA_HASH}&dn=Film",
        "seeders": 42,
        "peers": 45,
    },
    {
        "title": "Series 2020 S01 720p",
        "link": f"magnet:?xt=urn:btih:{SINGLE_HASH}&dn=Series",
        "pubDate": "Fri, 20 Nov 2020 08:30:00 +0000",
        "size": 12992276480,
        "indexer": "FakeIndexer",
        "magnet": None,
        "seeders": 7,
        "peers": 8,
    },
]


def rutor_ls() -> bytes:
    compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
    payload = json.dumps(RUTOR, ensure_ascii=False).encode("utf-8")
    return compressor.compress(payload) + compressor.flush()


def main() -> None:
    target = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    target.mkdir(parents=True, exist_ok=True)
    (target / "rutor.ls").write_bytes(rutor_ls())


if __name__ == "__main__":
    main()
