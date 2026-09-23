#!/usr/bin/env python3
"""Observe what a target keeps across a process restart.

One sequence runs against one target: an unsaved and a saved torrent, viewed
entries for both, changed settings and WAF lists. The target container is then
restarted and the observable state is recorded. Only fields that are stable
across runs are kept, so the reference and candidate reports compare as JSON.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import time
from typing import Any

from run import request, restart_target, valid_transport, wait_for_torrent

SCHEMA = "rustorr.r6.restart-matrix.v1"


def call(base_url: str, variables: dict[str, str], step: dict[str, Any]) -> dict[str, Any]:
    result = request(base_url, {"auth": True, **step}, variables, 30)
    if not valid_transport(result) or result["response"]["status"] != step.get("expect_status", 200):
        raise RuntimeError(f"{step['id']}: {result.get('error') or result['response']['status']}")
    return result["response"].get("json")


def catalog(base_url: str, variables: dict[str, str]) -> list[dict[str, Any]]:
    listed = call(base_url, variables, {"id": "list", "method": "POST", "path": "/torrents", "json": {"action": "list"}})
    return sorted(
        ({key: item.get(key) for key in ("hash", "title", "category", "data")} for item in listed or []),
        key=lambda item: item["hash"],
    )


def viewed(base_url: str, variables: dict[str, str]) -> list[dict[str, Any]]:
    listed = call(base_url, variables, {"id": "viewed", "method": "POST", "path": "/viewed", "json": {"action": "list"}})
    return sorted(listed or [], key=lambda item: (item.get("hash"), item.get("file_index")))


def observe(base_url: str, variables: dict[str, str]) -> dict[str, Any]:
    return {
        "catalog": catalog(base_url, variables),
        "viewed": viewed(base_url, variables),
        "settings": call(base_url, variables, {"id": "settings", "method": "POST", "path": "/settings", "json": {"action": "get"}}),
        "waf": call(base_url, variables, {"id": "waf", "method": "GET", "path": "/waf"}),
    }


def reset(base_url: str, variables: dict[str, str]) -> None:
    call(base_url, variables, {"id": "settings-def", "method": "POST", "path": "/settings", "json": {"action": "def"}})
    call(base_url, variables, {"id": "waf-clear", "method": "POST", "path": "/waf", "json": {"whitelist": "", "blacklist": "", "referers": ""}})
    for name in ("unsaved_hash", "saved_hash"):
        call(base_url, variables, {"id": "viewed-clear", "method": "POST", "path": "/viewed", "json": {"action": "rem", "hash": variables[name], "file_index": -1}})
    call(base_url, variables, {"id": "wipe", "method": "POST", "path": "/torrents", "json": {"action": "wipe"}})


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--target-container", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--docker-socket", default="/var/run/docker.sock")
    parser.add_argument("--readiness-timeout", type=float, default=90)
    args = parser.parse_args()

    variables = {
        "unsaved_link": "file:///fixtures/torrents/single.torrent",
        "unsaved_hash": "68c3ccdd52b2925f4f97e2f61ea248e728e54cea",
        "saved_link": "file:///fixtures/torrents/unicode.torrent",
        "saved_hash": "d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d",
    }
    report: dict[str, Any] = {"schema": SCHEMA, "valid": False}
    try:
        reset(args.base_url, variables)
        call(args.base_url, variables, {"id": "add-unsaved", "method": "POST", "path": "/torrents", "json": {"action": "add", "link": variables["unsaved_link"], "title": "unsaved"}})
        call(args.base_url, variables, {"id": "add-saved", "method": "POST", "path": "/torrents", "json": {"action": "add", "link": variables["saved_link"], "save_to_db": True, "title": "saved", "category": "restart", "data": "opaque"}})
        for name in ("unsaved_hash", "saved_hash"):
            _, failure = wait_for_torrent(args.base_url, variables[name], variables, args.readiness_timeout, require_playback=False)
            if failure:
                raise RuntimeError(failure)
            call(args.base_url, variables, {"id": "viewed-set", "method": "POST", "path": "/viewed", "json": {"action": "set", "hash": variables[name], "file_index": 1, "timecode": 42}})
        settings = call(args.base_url, variables, {"id": "settings-get", "method": "POST", "path": "/settings", "json": {"action": "get"}})
        settings.update({"CacheSize": 50331648, "PreloadCache": 30, "TorrentDisconnectTimeout": 45, "MergeAllM3U": True})
        call(args.base_url, variables, {"id": "settings-set", "method": "POST", "path": "/settings", "json": {"action": "set", "sets": settings}})
        # 192.0.2.0/24 is TEST-NET-1: persisted, but it never matches this probe.
        call(args.base_url, variables, {"id": "waf-set", "method": "POST", "path": "/waf", "json": {"whitelist": "", "blacklist": "192.0.2.0/24", "referers": "restart.invalid"}})

        report["before"] = observe(args.base_url, variables)
        restarted = restart_target(args.docker_socket, args.target_container, args.base_url, variables, args.readiness_timeout, None)
        if restarted.get("status") is None:
            raise RuntimeError(f"restart: {restarted.get('error')}")
        # A saved torrent may be restored lazily; give it a bounded interval.
        deadline = time.monotonic() + args.readiness_timeout
        while True:
            after = observe(args.base_url, variables)
            if any(item["hash"] == variables["saved_hash"] for item in after["catalog"]) or time.monotonic() > deadline:
                break
            time.sleep(0.5)
        report["after"] = after
        report["valid"] = True
    except RuntimeError as exc:
        report["error"] = str(exc)
    finally:
        try:
            reset(args.base_url, variables)
        except RuntimeError as exc:
            report.setdefault("error", f"cleanup: {exc}")
            report["valid"] = False
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if not report["valid"]:
        sys.exit(f"error: {report['error']}")


if __name__ == "__main__":
    main()
