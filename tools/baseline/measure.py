#!/usr/bin/env python3
"""Run the reproducible HTTP-level R1 workload matrix against TorrServer."""

from __future__ import annotations

import argparse
import json
import pathlib
import socket
import statistics
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

CHUNK = 256 * 1024
FILE_SIZE = 8 * 1024 * 1024


def http_request(base: str, path: str, method: str = "GET", headers: dict[str, str] | None = None, timeout: int = 30) -> dict:
    started = time.monotonic()
    req = urllib.request.Request(base.rstrip("/") + path, method=method)
    req.add_header("Accept", "application/json, */*")
    for key, value in (headers or {}).items():
        req.add_header(key, value)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            payload = response.read()
            return {"ok": True, "status": response.status, "bytes": len(payload), "content_range": response.headers.get("Content-Range"), "latency_ms": (time.monotonic() - started) * 1000}
    except (urllib.error.URLError, socket.timeout, TimeoutError, OSError) as error:
        return {"ok": False, "error": str(error), "latency_ms": (time.monotonic() - started) * 1000}


def post_json(base: str, path: str, value: dict) -> dict:
    started = time.monotonic()
    req = urllib.request.Request(base.rstrip("/") + path, data=json.dumps(value).encode("utf-8"), method="POST", headers={"Accept": "application/json", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            payload = response.read()
            decoded = json.loads(payload.decode("utf-8")) if payload else None
            return {"ok": True, "status": response.status, "bytes": len(payload), "latency_ms": (time.monotonic() - started) * 1000, "json": decoded}
    except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as error:
        return {"ok": False, "error": str(error), "latency_ms": (time.monotonic() - started) * 1000}


def torrent_query(link: str) -> str:
    return urllib.parse.urlencode({"link": link, "index": "1", "play": ""})


def range_request(base: str, link: str, start: int, timeout: int = 30) -> dict:
    result = http_request(base, "/stream?" + torrent_query(link), headers={"Range": f"bytes={start}-{min(FILE_SIZE - 1, start + CHUNK - 1)}"}, timeout=timeout)
    result["offset"] = start
    result["stall"] = (not result.get("ok")) or result.get("latency_ms", 0) >= 2000
    return result


def wait_for_torrent(base: str, expected_hash: str, timeout_s: int = 90) -> dict:
    started = time.monotonic()
    last = None
    while time.monotonic() - started < timeout_s:
        last = post_json(base, "/torrents", {"action": "list"})
        listed = last.get("json") if last.get("ok") else None
        if isinstance(listed, list) and any(item.get("hash") == expected_hash and item.get("stat", 0) >= 3 and item.get("connected_seeders", 0) >= 1 for item in listed):
            return {"ok": True, "metadata_discovery_ms": (time.monotonic() - started) * 1000, "last": last}
        time.sleep(1)
    return {"ok": False, "metadata_discovery_ms": (time.monotonic() - started) * 1000, "last": last}


def prepare(base: str, link: str, expected_hash: str, input_kind: str = "torrent") -> list[dict]:
    events = [{"scenario": "prepare-wipe", **post_json(base, "/torrents", {"action": "wipe"})}]
    add_link = link if input_kind == "torrent" else f"magnet:?xt=urn:btih:{expected_hash}&dn=single&tr={urllib.parse.quote('http://tracker:6969/announce', safe='')}"
    events.append({"scenario": "prepare-add", "input": input_kind, **post_json(base, "/torrents", {"action": "add", "link": add_link})})
    metadata = wait_for_torrent(base, expected_hash)
    events.append({"scenario": "prepare-metadata", "input": input_kind, **metadata})
    return events


def docker_exec(container: str, *command: str) -> dict:
    result = subprocess.run(["docker", "exec", container, *command], capture_output=True, text=True, check=False)
    return {"ok": result.returncode == 0, "returncode": result.returncode, "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}


def apply_netem(container: str, delay_ms: int, loss_percent: int) -> dict:
    return docker_exec(container, "tc", "qdisc", "replace", "dev", "eth0", "root", "netem", "delay", f"{delay_ms}ms", "loss", f"{loss_percent}%")


def clear_netem(container: str) -> dict:
    return docker_exec(container, "tc", "qdisc", "del", "dev", "eth0", "root")


def run_scenario(base: str, scenario: dict, link: str, expected_hash: str, torrserver_container: str | None, seeder_container: str | None) -> list[dict]:
    scenario_id = scenario["id"]
    events: list[dict] = []
    if scenario_id == "cold-known-torrent":
        events.extend(prepare(base, link, expected_hash))
        event = range_request(base, link, 0)
        events.append({"scenario": scenario_id, "client_start_ms": event.get("latency_ms"), "server_start_ms": None, **event})
    elif scenario_id == "cold-magnet-metadata":
        started = time.monotonic()
        events.extend(prepare(base, link, expected_hash, "magnet"))
        events.append({"scenario": scenario_id, "client_start_ms": (time.monotonic() - started) * 1000, "metadata_discovery_ms": next((e.get("metadata_discovery_ms") for e in events if e["scenario"] == "prepare-metadata"), None), "stalls": 0})
    elif scenario_id == "warm-known-torrent":
        event = range_request(base, link, 0)
        events.append({"scenario": scenario_id, "client_start_ms": event.get("latency_ms"), **event})
    elif scenario_id in {"seek-loaded", "seek-missing", "seek-evicted"}:
        if scenario_id in {"seek-missing", "seek-evicted"}:
            if scenario_id == "seek-missing":
                events.append({"scenario": "missing-precondition", "note": "wipe/add followed by a first-range read leaves the seek target outside the requested area"})
            else:
                events.append({"scenario": "eviction-precondition", "note": "wipe/add establishes a cold piece map; TorrServer exposes no deterministic eviction API in this harness"})
            events.extend(prepare(base, link, expected_hash))
        first = range_request(base, link, 0)
        second = range_request(base, link, FILE_SIZE // 2)
        events.append({"scenario": scenario_id, "state": scenario["state"], "seek_ms": second.get("latency_ms"), "stalls": int(second.get("stall", False)), "initial": first, "seek": second})
    elif scenario_id.startswith("continuous-"):
        views = scenario["views"]
        offsets = [i * CHUNK for i in range(8)]
        started = time.monotonic()
        with ThreadPoolExecutor(max_workers=views) as pool:
            results = list(pool.map(lambda offset: range_request(base, link, offset), offsets * views))
        events.append({"scenario": scenario_id, "views": views, "duration_ms": (time.monotonic() - started) * 1000, "stalls": sum(int(r.get("stall", False)) for r in results), "ranges": results})
    elif scenario_id == "netem-delay-loss":
        if not torrserver_container:
            return [{"scenario": scenario_id, "status": "blocked", "error": "--torrserver-container is required"}]
        network = scenario["network"]
        applied = apply_netem(torrserver_container, network["delay_ms"], network["loss_percent"])
        try:
            result = range_request(base, link, FILE_SIZE // 2)
        finally:
            cleared = clear_netem(torrserver_container)
        events.append({"scenario": scenario_id, "status": "controlled", "netem_apply": applied, "netem_clear": cleared, **result})
    elif scenario_id == "peer-departure":
        if not seeder_container:
            return [{"scenario": scenario_id, "status": "blocked", "error": "--seeder-container is required"}]
        initial = range_request(base, link, 0)
        stopped = subprocess.run(["docker", "stop", seeder_container], capture_output=True, text=True, check=False)
        after_departure = range_request(base, link, FILE_SIZE // 2)
        events.append({"scenario": scenario_id, "status": "controlled", "seeder_stop": {"ok": stopped.returncode == 0, "returncode": stopped.returncode, "stdout": stopped.stdout.strip(), "stderr": stopped.stderr.strip()}, "recovery_ms": after_departure.get("latency_ms"), "initial": initial, "after_departure": after_departure, "stalls": int(initial.get("stall", False)) + int(after_departure.get("stall", False))})
    return events


def aggregate(events: list[dict]) -> dict:
    latencies = [e["latency_ms"] for e in events if isinstance(e.get("latency_ms"), (int, float))]
    stalls = sum(int(e.get("stall", False)) for e in events) + sum(int(e.get("stalls", 0)) for e in events)
    ordered = sorted(latencies)
    pick = lambda fraction: ordered[min(len(ordered) - 1, round((len(ordered) - 1) * fraction))] if ordered else None
    return {"requests": len(latencies), "failed_requests": sum(e.get("ok") is False for e in events), "stalls": stalls, "latency_ms": {"p50": pick(.5), "p95": pick(.95), "mean": statistics.mean(latencies) if latencies else None}}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8090")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--known-link", required=True)
    parser.add_argument("--known-hash", required=True)
    parser.add_argument("--scenarios", type=pathlib.Path, default=pathlib.Path(__file__).with_name("scenarios.json"))
    parser.add_argument("--torrserver-container")
    parser.add_argument("--seeder-container")
    args = parser.parse_args()
    matrix = json.loads(args.scenarios.read_text(encoding="utf-8"))
    events: list[dict] = []
    for scenario in matrix["scenarios"]:
        events.extend(run_scenario(args.base_url, scenario, args.known_link, args.known_hash, args.torrserver_container, args.seeder_container))
    result = {"schema": "rustorr.r1.measurement.v2", "reference": matrix["reference"], "scenarios": matrix, "events": events, "summary": aggregate(events)}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
