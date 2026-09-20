#!/usr/bin/env python3
"""Capture a reproducible HTTP contract corpus from TorrServer or Rustorr."""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import json
import pathlib
import re
import socket
import ssl
import time
import urllib.error
import urllib.request
from typing import Any

TOKEN = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


def substitute(value: Any, variables: dict[str, str]) -> Any:
    if isinstance(value, str):
        return TOKEN.sub(lambda match: variables.get(match.group(1), match.group(0)), value)
    if isinstance(value, dict):
        return {key: substitute(item, variables) for key, item in value.items()}
    if isinstance(value, list):
        return [substitute(item, variables) for item in value]
    return value


def request(base_url: str, scenario: dict[str, Any], variables: dict[str, str], timeout: float, context: ssl.SSLContext | None = None) -> dict[str, Any]:
    resolved = substitute(scenario, variables)
    body: bytes | None = None
    headers = {str(key): str(value) for key, value in resolved.get("headers", {}).items()}
    if resolved.get("auth") and variables.get("basic_auth"):
        headers["Authorization"] = "Basic " + base64.b64encode(variables["basic_auth"].encode()).decode()
    if "json" in resolved:
        body = json.dumps(resolved["json"], separators=(",", ":")).encode()
        headers.setdefault("Content-Type", "application/json")
    elif "raw_body" in resolved:
        body = str(resolved["raw_body"]).encode()
    headers.setdefault("Accept", "application/json, text/plain, */*")
    path = str(resolved["path"])
    url = base_url.rstrip("/") + path
    started = time.monotonic()
    req = urllib.request.Request(url, data=body, headers=headers, method=str(resolved["method"]))
    status: int | None = None
    response_headers: dict[str, str] = {}
    payload = b""
    error: str | None = None
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=context) as response:
            status = response.status
            response_headers = dict(response.headers.items())
            payload = response.read()
    except urllib.error.HTTPError as exc:
        status = exc.code
        response_headers = dict(exc.headers.items())
        payload = exc.read()
    except http.client.IncompleteRead as exc:
        payload = exc.partial
        error = f"incomplete response: {exc}"
    except (urllib.error.URLError, socket.timeout, TimeoutError, OSError) as exc:
        error = str(exc)
    elapsed = (time.monotonic() - started) * 1000
    result: dict[str, Any] = {
        "id": resolved["id"],
        "area": resolved.get("area", "unknown"),
        "classification": resolved.get("classification", "not-characterized"),
        "request": {"method": resolved["method"], "url": url, "headers": headers, "body": body.decode("utf-8", "replace") if body else None},
        "response": {
            "status": status,
            "headers": dict(sorted(response_headers.items(), key=lambda pair: pair[0].lower())),
            "body_sha256": hashlib.sha256(payload).hexdigest(),
            "body_bytes": len(payload),
            "body_base64": base64.b64encode(payload).decode("ascii"),
        },
        "latency_ms": round(elapsed, 3),
    }
    if error:
        result["error"] = error
    try:
        result["response"]["json"] = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        pass
    return result


def wait_for_torrent(base_url: str, variables: dict[str, str], timeout: float, context: ssl.SSLContext | None = None) -> dict[str, Any]:
    """Wait for the deterministic fixture to be readable before stream probes."""
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        probe = request(base_url, {"id": "setup-torrent-list", "area": "setup", "method": "POST", "path": "/torrents", "json": {"action": "list"}}, variables, min(3, timeout), context)
        last = probe
        entries = probe.get("response", {}).get("json")
        if isinstance(entries, list) and any(item.get("hash") == variables["torrent_hash"] and item.get("stat", 0) >= 3 and item.get("connected_seeders", 0) >= 1 for item in entries if isinstance(item, dict)):
            return {"ready": True, "last": probe}
        time.sleep(1)
    return {"ready": False, "last": last}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8090")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--manifest", type=pathlib.Path, default=pathlib.Path(__file__).with_name("scenarios.json"))
    parser.add_argument("--torrent-link")
    parser.add_argument("--torrent-hash")
    parser.add_argument("--basic-auth", help="user:password for scenarios marked auth")
    parser.add_argument("--only", help="comma-separated scenario IDs to capture")
    parser.add_argument("--timeout", type=float, default=5)
    parser.add_argument("--readiness-timeout", type=float, default=90)
    parser.add_argument("--insecure", action="store_true", help="disable certificate verification for local TLS probes")
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    variables = {**manifest.get("defaults", {}), "torrent_link": args.torrent_link or manifest["defaults"]["torrent_link"], "torrent_hash": args.torrent_hash or manifest["defaults"]["torrent_hash"]}
    if args.basic_auth:
        variables["basic_auth"] = args.basic_auth
    context = ssl._create_unverified_context() if args.insecure else None
    selected = set(args.only.split(",")) if args.only else None
    scenarios = [scenario for scenario in manifest["scenarios"] if selected is None or scenario["id"] in selected]
    cases = []
    setup: list[dict[str, Any]] = []
    for scenario in scenarios:
        case = request(args.base_url, scenario, variables, args.timeout, context)
        cases.append(case)
        if scenario["id"] in {"torrents-add-known", "torrents-add-media"}:
            readiness_variables = dict(variables)
            if scenario["id"] == "torrents-add-media":
                readiness_variables["torrent_hash"] = variables["media_hash"]
            readiness = wait_for_torrent(args.base_url, readiness_variables, args.readiness_timeout, context)
            setup.append({"after": scenario["id"], "torrent_readiness": readiness})
    result = {"schema": "rustorr.r2.contract-corpus.v1", "reference": manifest["reference"], "manifest": str(args.manifest), "normalization": manifest.get("normalization", {}), "setup": setup, "cases": cases}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(args.output)


if __name__ == "__main__":
    main()
