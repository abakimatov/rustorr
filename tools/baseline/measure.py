#!/usr/bin/env python3
"""Run the reproducible HTTP-level R1 workload matrix against TorrServer."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from fixture_payload import payload_range

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


def post_json(base: str, path: str, value: dict, timeout: int = 30) -> dict:
    started = time.monotonic()
    req = urllib.request.Request(base.rstrip("/") + path, data=json.dumps(value).encode("utf-8"), method="POST", headers={"Accept": "application/json", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            payload = response.read()
            decoded = json.loads(payload.decode("utf-8")) if payload else None
            return {"ok": True, "status": response.status, "bytes": len(payload), "latency_ms": (time.monotonic() - started) * 1000, "json": decoded}
    except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as error:
        return {"ok": False, "error": str(error), "latency_ms": (time.monotonic() - started) * 1000}


def torrent_query(link: str) -> str:
    return urllib.parse.urlencode({"link": link, "index": "1", "play": ""})


def range_request(base: str, link: str, start: int, timeout: int = 30) -> dict:
    end = min(FILE_SIZE - 1, start + CHUNK - 1)
    expected_length = end - start + 1
    url = base.rstrip("/") + "/stream?" + torrent_query(link)
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="rustorr-range-") as directory:
        root = pathlib.Path(directory)
        headers_path = root / "headers"
        body_path = root / "body"
        command = [
            "curl",
            "--silent",
            "--show-error",
            "--max-time",
            str(timeout),
            "--header",
            f"Range: bytes={start}-{end}",
            "--dump-header",
            str(headers_path),
            "--output",
            str(body_path),
            "--write-out",
            "%{http_code}\n%{time_total}",
            url,
        ]
        try:
            completed = subprocess.run(
                command,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout + 5,
            )
        except subprocess.TimeoutExpired as error:
            return {
                "ok": False,
                "error": f"curl subprocess exceeded {timeout + 5}s: {error}",
                "latency_ms": (time.monotonic() - started) * 1000,
                "offset": start,
                "stall": True,
            }

        output = completed.stdout.strip().splitlines()
        status = int(output[-2]) if len(output) >= 2 and output[-2].isdigit() else None
        try:
            curl_time_ms = float(output[-1]) * 1000 if output else None
        except ValueError:
            curl_time_ms = None
        body = body_path.read_bytes() if body_path.exists() else b""
        header_lines = headers_path.read_text(encoding="iso-8859-1").splitlines() if headers_path.exists() else []
        content_range = next(
            (line.split(":", 1)[1].strip() for line in reversed(header_lines) if line.lower().startswith("content-range:")),
            None,
        )
        expected = payload_range(start, expected_length)
        digest = hashlib.sha256(body).hexdigest()
        expected_digest = hashlib.sha256(expected).hexdigest()
        integrity_errors = []
        if status != 206:
            integrity_errors.append(f"expected HTTP 206, got {status}")
        wanted_content_range = f"bytes {start}-{end}/{FILE_SIZE}"
        if content_range != wanted_content_range:
            integrity_errors.append(f"expected Content-Range {wanted_content_range!r}, got {content_range!r}")
        if len(body) != expected_length:
            integrity_errors.append(f"expected {expected_length} bytes, got {len(body)}")
        if digest != expected_digest:
            integrity_errors.append(f"expected SHA-256 {expected_digest}, got {digest}")
        if completed.returncode != 0:
            integrity_errors.append(f"curl exited {completed.returncode}: {completed.stderr.strip()}")
        result = {
            "ok": not integrity_errors,
            "status": status,
            "bytes": len(body),
            "content_range": content_range,
            "sha256": digest,
            "expected_sha256": expected_digest,
            "integrity_errors": integrity_errors,
            "latency_ms": curl_time_ms if curl_time_ms is not None else (time.monotonic() - started) * 1000,
            "wall_ms": (time.monotonic() - started) * 1000,
            "offset": start,
        }
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


def prepare(
    base: str,
    link: str,
    expected_hash: str,
    input_kind: str = "torrent",
    save_to_db: bool = False,
) -> list[dict]:
    events = [{"scenario": "prepare-wipe", **post_json(base, "/torrents", {"action": "wipe"})}]
    if not events[-1].get("ok"):
        return events
    add_link = link if input_kind == "torrent" else f"magnet:?xt=urn:btih:{expected_hash}&dn=single&tr={urllib.parse.quote('http://tracker:6969/announce', safe='')}"
    add_timeout = 90 if input_kind == "magnet" else 30
    events.append(
        {
            "scenario": "prepare-add",
            "input": input_kind,
            **post_json(
                base,
                "/torrents",
                {"action": "add", "link": add_link, "save_to_db": save_to_db},
                timeout=add_timeout,
            ),
        }
    )
    if not events[-1].get("ok"):
        return events
    metadata = wait_for_torrent(base, expected_hash)
    events.append({"scenario": "prepare-metadata", "input": input_kind, **metadata})
    return events


def docker_exec(container: str, *command: str) -> dict:
    try:
        result = subprocess.run(
            ["docker", "exec", container, *command],
            capture_output=True,
            text=True,
            check=False,
            timeout=35,
        )
        return {"ok": result.returncode == 0, "returncode": result.returncode, "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}
    except subprocess.TimeoutExpired as error:
        return {"ok": False, "error": f"docker exec timed out: {error}"}


def run_netem(container: str, image: str, *command: str) -> dict:
    try:
        result = subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "--network",
                f"container:{container}",
                "--cap-add",
                "NET_ADMIN",
                "--entrypoint",
                "tc",
                image,
                *command,
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=35,
        )
        return {"ok": result.returncode == 0, "returncode": result.returncode, "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}
    except subprocess.TimeoutExpired as error:
        return {"ok": False, "error": f"netem helper timed out: {error}"}


def apply_netem(container: str, image: str, delay_ms: int, loss_percent: int) -> dict:
    return run_netem(container, image, "qdisc", "replace", "dev", "eth0", "root", "netem", "delay", f"{delay_ms}ms", "loss", f"{loss_percent}%")


def clear_netem(container: str, image: str) -> dict:
    return run_netem(container, image, "qdisc", "del", "dev", "eth0", "root")


def control_evict(socket_path: str | None, torrent_hash: str) -> dict:
    if not socket_path:
        return {"ok": False, "error": "--control-socket is required for deterministic eviction"}
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(30)
            connection.connect(socket_path)
            connection.sendall(f"evict {torrent_hash}\n".encode("ascii"))
            response = connection.recv(1024).decode("utf-8", "replace").strip()
        return {"ok": response.startswith("ok "), "response": response}
    except OSError as error:
        return {"ok": False, "error": str(error)}


def run_scenario(base: str, scenario: dict, link: str, expected_hash: str, torrserver_container: str | None, seeder_container: str | None, control_socket: str | None, netem_image: str) -> list[dict]:
    scenario_id = scenario["id"]
    events: list[dict] = []
    if scenario_id == "cold-known-torrent":
        events.extend(prepare(base, link, expected_hash))
        if any(event.get("ok") is False for event in events):
            return events
        event = range_request(base, link, 0)
        events.append({"scenario": scenario_id, "client_start_ms": event.get("latency_ms"), "server_start_ms": None, **event})
    elif scenario_id == "cold-magnet-metadata":
        started = time.monotonic()
        events.extend(prepare(base, link, expected_hash, "magnet"))
        if any(event.get("ok") is False for event in events):
            return events
        metadata_elapsed_ms = (time.monotonic() - started) * 1000
        magnet = f"magnet:?xt=urn:btih:{expected_hash}&dn=single&tr={urllib.parse.quote('http://tracker:6969/announce', safe='')}"
        first = range_request(base, magnet, 0)
        events.append({
            "scenario": scenario_id,
            "client_start_ms": metadata_elapsed_ms,
            "metadata_discovery_ms": next((e.get("metadata_discovery_ms") for e in events if e["scenario"] == "prepare-metadata"), None),
            "magnet_first_range_ms": first.get("latency_ms"),
            "stalls": int(first.get("stall", False)),
            "first_range": first,
        })
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
            if any(event.get("ok") is False for event in events):
                return events
        first = range_request(base, link, 0)
        if not first.get("ok"):
            events.append({"scenario": scenario_id, "state": scenario["state"], "initial": first, "status": "precondition-failed"})
            return events
        if scenario_id == "seek-evicted" and control_socket:
            eviction = {"scenario": "eviction-control", **control_evict(control_socket, expected_hash)}
            events.append(eviction)
            if not eviction.get("ok"):
                return events
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
        applied = apply_netem(torrserver_container, netem_image, network["delay_ms"], network["loss_percent"])
        if not applied.get("ok"):
            events.append({"scenario": scenario_id, "status": "precondition-failed", "netem_apply": applied})
            return events
        results = []
        try:
            for _ in range(scenario.get("samples", 1)):
                result = range_request(base, link, FILE_SIZE // 2)
                results.append(result)
                if not result.get("ok"):
                    break
        finally:
            cleared = clear_netem(torrserver_container, netem_image)
        events.append({
            "scenario": scenario_id,
            "status": "controlled",
            "sample_count": len(results),
            "netem_apply": applied,
            "netem_clear": cleared,
            "ranges": results,
            "stalls": sum(int(result.get("stall", False)) for result in results),
        })
    elif scenario_id == "peer-departure":
        if not seeder_container:
            return [{"scenario": scenario_id, "status": "blocked", "error": "--seeder-container is required"}]
        initial = range_request(base, link, 0)
        if not initial.get("ok"):
            return [{"scenario": scenario_id, "status": "precondition-failed", "initial": initial}]
        try:
            stopped = subprocess.run(["docker", "stop", seeder_container], capture_output=True, text=True, check=False, timeout=35)
        except subprocess.TimeoutExpired as error:
            return [{"scenario": scenario_id, "status": "precondition-failed", "initial": initial, "seeder_stop": {"ok": False, "error": str(error)}}]
        if stopped.returncode != 0:
            return [{"scenario": scenario_id, "status": "precondition-failed", "initial": initial, "seeder_stop": {"ok": False, "returncode": stopped.returncode, "stdout": stopped.stdout.strip(), "stderr": stopped.stderr.strip()}}]
        after_departure = range_request(base, link, FILE_SIZE // 2)
        events.append({"scenario": scenario_id, "status": "controlled", "seeder_stop": {"ok": stopped.returncode == 0, "returncode": stopped.returncode, "stdout": stopped.stdout.strip(), "stderr": stopped.stderr.strip()}, "recovery_ms": after_departure.get("latency_ms"), "initial": initial, "after_departure": after_departure, "stalls": int(initial.get("stall", False)) + int(after_departure.get("stall", False))})
    return events


def aggregate(events: list[dict]) -> dict:
    def dictionaries(node: object):
        if isinstance(node, dict):
            yield node
            for child in node.values():
                yield from dictionaries(child)
        elif isinstance(node, list):
            for child in node:
                yield from dictionaries(child)

    nodes = list(dictionaries(events))
    latencies = [node["latency_ms"] for node in nodes if isinstance(node.get("latency_ms"), (int, float))]
    stalls = sum(int(e.get("stall", False)) for e in events) + sum(int(e.get("stalls", 0)) for e in events)
    ordered = sorted(latencies)
    pick = lambda fraction: ordered[min(len(ordered) - 1, round((len(ordered) - 1) * fraction))] if ordered else None
    return {
        "requests": len(latencies),
        "failed_requests": sum(node.get("ok") is False for node in nodes),
        "integrity_errors": sum(len(node.get("integrity_errors", [])) for node in nodes),
        "stalls": stalls,
        "latency_ms": {"p50": pick(.5), "p95": pick(.95), "mean": statistics.mean(latencies) if latencies else None},
    }


def failure_reason(events: list[dict]) -> str | None:
    def visit(node: object):
        if isinstance(node, dict):
            yield node
            for child in node.values():
                yield from visit(child)
        elif isinstance(node, list):
            for child in node:
                yield from visit(child)

    for node in visit(events):
        if node.get("status") in {"blocked", "precondition-failed"}:
            return f"{node.get('scenario', 'scenario')}: {node.get('status')}"
        if node.get("ok") is False:
            return f"{node.get('scenario', 'operation')}: {node.get('error') or node.get('integrity_errors') or 'failed'}"
    return None


def atomic_write(path: pathlib.Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8090")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--known-link", required=True)
    parser.add_argument("--known-hash", required=True)
    parser.add_argument("--scenarios", type=pathlib.Path, default=pathlib.Path(__file__).with_name("scenarios.json"))
    parser.add_argument("--torrserver-container")
    parser.add_argument("--seeder-container")
    parser.add_argument("--control-socket")
    parser.add_argument("--netem-image", default="rustorr-r1-fixture")
    args = parser.parse_args()
    matrix = json.loads(args.scenarios.read_text(encoding="utf-8"))
    events: list[dict] = []
    result = {
        "schema": "rustorr.r1.measurement.v3",
        "reference": matrix["reference"],
        "scenarios": matrix,
        "events": events,
        "summary": aggregate(events),
        "complete": False,
        "last_scenario": None,
        "stop_reason": None,
    }
    atomic_write(args.output, result)
    try:
        for scenario in matrix["scenarios"]:
            result["last_scenario"] = scenario["id"]
            scenario_events = run_scenario(
                args.base_url,
                scenario,
                args.known_link,
                args.known_hash,
                args.torrserver_container,
                args.seeder_container,
                args.control_socket,
                args.netem_image,
            )
            events.extend(scenario_events)
            result["summary"] = aggregate(events)
            result["stop_reason"] = failure_reason(scenario_events)
            atomic_write(args.output, result)
            if result["stop_reason"]:
                raise RuntimeError(result["stop_reason"])
        result["complete"] = True
        result["stop_reason"] = None
        atomic_write(args.output, result)
    except KeyboardInterrupt:
        result["stop_reason"] = "interrupted"
        result["summary"] = aggregate(events)
        atomic_write(args.output, result)
        raise
    except Exception as error:
        if not result["stop_reason"]:
            result["stop_reason"] = f"{type(error).__name__}: {error}"
        result["summary"] = aggregate(events)
        atomic_write(args.output, result)
        raise


if __name__ == "__main__":
    main()
