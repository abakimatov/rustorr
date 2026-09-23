#!/usr/bin/env python3
"""Capture an isolated, reproducible HTTP contract corpus."""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import json
import mimetypes
import pathlib
import re
import socket
import ssl
import sys
import time
import urllib.error
import urllib.request
import uuid
from typing import Any

TOKEN = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


class UnixHTTPConnection(http.client.HTTPConnection):
    def __init__(self, socket_path: str, timeout: float):
        super().__init__("localhost", timeout=timeout)
        self.socket_path = socket_path

    def connect(self) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.socket_path)


def docker_api(socket_path: str, method: str, path: str, timeout: float) -> tuple[int, bytes]:
    connection = UnixHTTPConnection(socket_path, timeout)
    try:
        connection.request(method, path)
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


def substitute(value: Any, variables: dict[str, str]) -> Any:
    if isinstance(value, str):
        return TOKEN.sub(lambda match: variables.get(match.group(1), match.group(0)), value)
    if isinstance(value, dict):
        return {key: substitute(item, variables) for key, item in value.items()}
    if isinstance(value, list):
        return [substitute(item, variables) for item in value]
    return value


def multipart_body(spec: dict[str, Any]) -> tuple[bytes, str]:
    boundary = f"rustorr-contract-{uuid.uuid4().hex}"
    chunks: list[bytes] = []
    for name, value in spec.get("fields", {}).items():
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode(),
                str(value).encode(),
                b"\r\n",
            ]
        )
    for item in spec.get("files", []):
        path = pathlib.Path(item["path"])
        content_type = item.get("content_type") or mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                (
                    f'Content-Disposition: form-data; name="{item.get("field", "file")}"; '
                    f'filename="{item.get("filename", path.name)}"\r\n'
                ).encode(),
                f"Content-Type: {content_type}\r\n\r\n".encode(),
                path.read_bytes(),
                b"\r\n",
            ]
        )
    chunks.append(f"--{boundary}--\r\n".encode())
    return b"".join(chunks), f"multipart/form-data; boundary={boundary}"


def request(
    base_url: str,
    scenario: dict[str, Any],
    variables: dict[str, str],
    timeout: float,
    context: ssl.SSLContext | None = None,
) -> dict[str, Any]:
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
        if "content_type" in resolved:
            headers.setdefault("Content-Type", str(resolved["content_type"]))
    elif "multipart" in resolved:
        body, content_type = multipart_body(resolved["multipart"])
        headers.setdefault("Content-Type", content_type)
    headers.setdefault("Accept", "application/json, text/plain, */*")
    url = base_url.rstrip("/") + str(resolved["path"])
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
        "area": resolved.get("area", "setup"),
        "classification": resolved.get("classification", "support"),
        "request": {
            "method": resolved["method"],
            "url": url,
            "headers": headers,
            "body_sha256": hashlib.sha256(body or b"").hexdigest(),
            "body_bytes": len(body or b""),
        },
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


def valid_transport(result: dict[str, Any]) -> bool:
    return "error" not in result and result.get("response", {}).get("status") is not None


def transmission_rpc(
    url: str, method: str, timeout: float, arguments: dict[str, Any] | None = None
) -> dict[str, Any]:
    body = json.dumps({"method": method, "arguments": arguments or {}}).encode()
    headers = {"Content-Type": "application/json"}
    for attempt in range(2):
        req = urllib.request.Request(url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=timeout) as response:
                document = json.loads(response.read().decode("utf-8"))
                if document.get("result") != "success":
                    raise RuntimeError(f"Transmission {method} failed: {document!r}")
                return document
        except urllib.error.HTTPError as exc:
            session = exc.headers.get("X-Transmission-Session-Id")
            if exc.code != 409 or not session or attempt != 0:
                raise
            headers["X-Transmission-Session-Id"] = session
    raise RuntimeError(f"Transmission {method} did not complete")


def reset_seeder(url: str, timeout: float) -> dict[str, Any]:
    started = time.monotonic()
    reset_epoch = int(time.time())
    try:
        transmission_rpc(url, "torrent-stop", timeout)
        stop_deadline = time.monotonic() + timeout
        while time.monotonic() < stop_deadline:
            stopped = transmission_rpc(
                url,
                "torrent-get",
                timeout,
                {"fields": ["status"]},
            ).get("arguments", {}).get("torrents", [])
            if stopped and all(torrent.get("status") == 0 for torrent in stopped):
                break
            time.sleep(0.25)
        else:
            raise RuntimeError("Transmission torrents did not stop before restart")
        transmission_rpc(url, "torrent-start-now", timeout)
        transmission_rpc(url, "torrent-reannounce", timeout)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            document = transmission_rpc(
                url,
                "torrent-get",
                timeout,
                {"fields": ["status", "percentDone", "trackerStats"]},
            )
            torrents = document.get("arguments", {}).get("torrents", [])
            ready = torrents and all(
                torrent.get("status") == 6
                and torrent.get("percentDone") == 1
                and any(
                    tracker.get("lastAnnounceSucceeded")
                    and tracker.get("announceState") == 1
                    and tracker.get("lastAnnounceStartTime", 0) >= reset_epoch
                    for tracker in torrent.get("trackerStats", [])
                )
                for torrent in torrents
            )
            if ready:
                time.sleep(1)
                break
            time.sleep(0.5)
        else:
            raise RuntimeError("Transmission torrents did not return to announced seeding state")
        return {
            "id": "fixture-seeder-reset",
            "status": 200,
            "body_sha256": hashlib.sha256(b"").hexdigest(),
        }
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as exc:
        return {
            "id": "fixture-seeder-reset",
            "status": None,
            "body_sha256": hashlib.sha256(b"").hexdigest(),
            "error": str(exc),
            "latency_ms": round((time.monotonic() - started) * 1000, 3),
        }


def restart_seeder(
    socket_path: str, container: str, rpc_url: str, timeout: float
) -> dict[str, Any]:
    started = time.monotonic()
    reset_epoch = int(time.time())
    try:
        transmission_rpc(
            rpc_url,
            "torrent-remove",
            timeout,
            {"delete-local-data": False},
        )
        remove_deadline = time.monotonic() + timeout
        while time.monotonic() < remove_deadline:
            remaining = transmission_rpc(
                rpc_url,
                "torrent-get",
                timeout,
                {"fields": ["id"]},
            ).get("arguments", {}).get("torrents", [])
            if not remaining:
                break
            time.sleep(0.25)
        else:
            raise RuntimeError("Transmission torrents were not removed before restart")
        status, payload = docker_api(
            socket_path, "POST", f"/containers/{container}/restart?t=0", timeout
        )
        if status != 204:
            raise RuntimeError(f"Docker restart returned {status}: {payload.decode(errors='replace')}")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            status, payload = docker_api(
                socket_path, "GET", f"/containers/{container}/json", timeout
            )
            if status != 200:
                raise RuntimeError(f"Docker inspect returned {status}")
            state = json.loads(payload).get("State", {})
            if state.get("Running") and state.get("Health", {}).get("Status") == "healthy":
                try:
                    document = transmission_rpc(
                        rpc_url,
                        "torrent-get",
                        timeout,
                        {"fields": ["status", "percentDone", "trackerStats"]},
                    )
                except (OSError, urllib.error.URLError):
                    time.sleep(0.5)
                    continue
                torrents = document.get("arguments", {}).get("torrents", [])
                ready = torrents and all(
                    torrent.get("status") == 6
                    and torrent.get("percentDone") == 1
                    and any(
                        tracker.get("lastAnnounceSucceeded")
                        and tracker.get("announceState") == 1
                        and tracker.get("lastAnnounceStartTime", 0) >= reset_epoch
                        for tracker in torrent.get("trackerStats", [])
                    )
                    for torrent in torrents
                )
                if ready:
                    return {
                        "id": "fixture-seeder-restart",
                        "status": 200,
                        "body_sha256": hashlib.sha256(b"").hexdigest(),
                    }
            time.sleep(0.5)
        raise RuntimeError("seeder container did not become healthy and freshly announced")
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError, http.client.HTTPException) as exc:
        return {
            "id": "fixture-seeder-restart",
            "status": None,
            "body_sha256": hashlib.sha256(b"").hexdigest(),
            "error": str(exc),
            "latency_ms": round((time.monotonic() - started) * 1000, 3),
        }


def reset_tracker(
    socket_path: str,
    tracker_container: str,
    seeder_container: str,
    rpc_url: str,
    timeout: float,
) -> dict[str, Any]:
    """Drop stale peers, then rebuild the immutable seeder session."""
    started = time.monotonic()
    try:
        status, payload = docker_api(
            socket_path, "POST", f"/containers/{tracker_container}/restart?t=0", timeout
        )
        if status != 204:
            raise RuntimeError(
                f"Docker tracker restart returned {status}: {payload.decode(errors='replace')}"
            )
        seeder = restart_seeder(
            socket_path, seeder_container, rpc_url, timeout
        )
        if seeder["status"] is None:
            raise RuntimeError(seeder["error"])
        return {
            "id": "fixture-tracker-seeder-reset",
            "status": 200,
            "body_sha256": hashlib.sha256(b"").hexdigest(),
        }
    except (
        OSError,
        RuntimeError,
        ValueError,
        json.JSONDecodeError,
        http.client.HTTPException,
    ) as exc:
        return {
            "id": "fixture-tracker-seeder-reset",
            "status": None,
            "body_sha256": hashlib.sha256(b"").hexdigest(),
            "error": str(exc),
            "latency_ms": round((time.monotonic() - started) * 1000, 3),
        }


def restart_target(
    socket_path: str,
    container: str,
    base_url: str,
    variables: dict[str, str],
    timeout: float,
    context: ssl.SSLContext | None,
) -> dict[str, Any]:
    """Restart the target so peer backoff cannot leak between scenarios."""
    started = time.monotonic()
    try:
        status, payload = docker_api(
            socket_path, "POST", f"/containers/{container}/restart?t=0", timeout
        )
        if status != 204:
            raise RuntimeError(
                f"Docker target restart returned {status}: {payload.decode(errors='replace')}"
            )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            probe = request(
                base_url,
                {"id": "fixture-target-restart", "method": "GET", "path": "/echo", "auth": True},
                variables,
                min(timeout, 3),
                context,
            )
            if valid_transport(probe):
                time.sleep(2)
                return {
                    "id": "fixture-target-restart",
                    "status": probe["response"]["status"],
                    "body_sha256": probe["response"]["body_sha256"],
                }
            time.sleep(0.25)
        raise RuntimeError("target did not answer after restart")
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError, http.client.HTTPException) as exc:
        return {
            "id": "fixture-target-restart",
            "status": None,
            "body_sha256": hashlib.sha256(b"").hexdigest(),
            "error": str(exc),
            "latency_ms": round((time.monotonic() - started) * 1000, 3),
        }


def check_step(result: dict[str, Any], step: dict[str, Any]) -> str | None:
    if not valid_transport(result):
        detail = result.get("error", "status is null")
        return f"{result['id']}: transport error or null status: {detail}"
    expected = step.get("expect_status")
    if expected is None:
        return None
    allowed = {expected} if isinstance(expected, int) else set(expected)
    if result["response"]["status"] not in allowed:
        return f"{result['id']}: expected status {sorted(allowed)}, got {result['response']['status']}"
    return None


def capture_variables(result: dict[str, Any], step: dict[str, Any], variables: dict[str, str]) -> list[str]:
    failures: list[str] = []
    headers = {key.lower(): value for key, value in result["response"]["headers"].items()}
    document = result["response"].get("json")
    for name, source in step.get("capture", {}).items():
        value: Any = None
        if "header" in source:
            value = headers.get(str(source["header"]).lower())
        elif "json_path" in source:
            value = document
            for component in str(source["json_path"]).strip("/").split("/"):
                if component:
                    try:
                        value = value[int(component)] if isinstance(value, list) else value[component]
                    except (IndexError, KeyError, TypeError, ValueError):
                        value = None
                        break
        if value is None:
            failures.append(f"{result['id']}: cannot capture variable {name}")
        else:
            variables[name] = str(value)
    return failures


def wait_for_torrent(
    base_url: str,
    torrent_hash: str,
    variables: dict[str, str],
    timeout: float,
    context: ssl.SSLContext | None = None,
    require_playback: bool = True,
    require_data: bool = False,
) -> tuple[dict[str, Any], str | None]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        step = {
            "id": "setup-torrent-readiness",
            "method": "POST",
            "path": "/torrents",
            "json": {"action": "list"},
            "auth": True,
        }
        last = request(base_url, step, variables, min(3, timeout), context)
        if not valid_transport(last):
            return last, check_step(last, step)
        entries = last.get("response", {}).get("json")
        matching = next(
            (
                item
                for item in entries
                if isinstance(item, dict) and item.get("hash") == torrent_hash
            ),
            None,
        ) if isinstance(entries, list) else None
        # The reference writes a saved torrent's generated `data` after it
        # reports metadata, so a catalog observation must wait for it too.
        settled = not require_data or bool(matching and matching.get("data"))
        if matching and matching.get("stat", 0) >= 3 and settled:
            if not require_playback:
                return last, None
            if matching.get("connected_seeders", 0) >= 1:
                return last, None
            # A completed torrent may immediately disconnect its only seeder.
            # A verified byte-range is stronger readiness evidence than a
            # transient peer counter and avoids waiting for a peer that the
            # engine no longer needs. The reference blocks the range until a
            # peer connects, and a re-added torrent sometimes misses the first
            # dial, so a probe that times out is retried until the readiness
            # deadline; any response other than a transport timeout is final.
            while True:
                remaining = deadline - time.monotonic()
                probe = request(
                    base_url,
                    {
                        "id": "setup-peer-readiness",
                        "method": "GET",
                        "path": f"/play/{torrent_hash}/1",
                        "headers": {"Range": "bytes=0-0"},
                    },
                    variables,
                    max(1, min(30, remaining)),
                    context,
                )
                if valid_transport(probe) or deadline - time.monotonic() <= 0:
                    failure = check_step(probe, {"expect_status": 206})
                    if failure:
                        peers = {
                            key: matching.get(key)
                            for key in ("stat", "total_peers", "pending_peers", "active_peers", "connected_seeders")
                        }
                        failure = f"{failure}; last list entry {peers}"
                    return probe, failure
        time.sleep(0.5)
    return last, f"torrent {torrent_hash} did not become readable within {timeout}s"


def run_steps(
    base_url: str,
    steps: list[dict[str, Any]],
    variables: dict[str, str],
    timeout: float,
    readiness_timeout: float,
    context: ssl.SSLContext | None,
) -> tuple[list[dict[str, Any]], list[str]]:
    results: list[dict[str, Any]] = []
    failures: list[str] = []
    for number, raw_step in enumerate(steps):
        step = substitute(raw_step, variables)
        if "wait_for_torrent" in step or "wait_for_metadata" in step:
            metadata_only = "wait_for_metadata" in step
            result, failure = wait_for_torrent(
                base_url,
                str(step["wait_for_metadata" if metadata_only else "wait_for_torrent"]),
                variables,
                readiness_timeout,
                context,
                require_playback=not metadata_only,
                require_data=bool(step.get("require_data")),
            )
            result["id"] = step.get("id", f"wait-{number}")
            results.append(result)
            if failure:
                failures.append(failure)
            continue
        step.setdefault("id", f"step-{number}")
        result = request(base_url, step, variables, timeout, context)
        results.append(result)
        failure = check_step(result, step)
        if failure:
            failures.append(failure)
        failures.extend(capture_variables(result, step, variables))
    return results, failures


def summary(result: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": result["id"],
        "status": result["response"]["status"],
        "body_sha256": result["response"]["body_sha256"],
        **({"error": result["error"]} if "error" in result else {}),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8090")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--manifest", type=pathlib.Path, default=pathlib.Path(__file__).with_name("scenarios.json"))
    parser.add_argument("--torrent-link")
    parser.add_argument("--torrent-hash")
    parser.add_argument("--torrent-file", type=pathlib.Path)
    parser.add_argument("--basic-auth", help="user:password for setup and scenarios marked auth")
    parser.add_argument("--profile", choices=("direct", "auth", "proxy"), default="direct")
    parser.add_argument("--only", help="comma-separated scenario IDs to capture")
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--readiness-timeout", type=float, default=90)
    parser.add_argument("--seeder-rpc", help="Transmission RPC URL reset before every isolated scenario")
    parser.add_argument("--docker-socket", default="/var/run/docker.sock")
    parser.add_argument("--seeder-container", help="Docker seeder container restarted before every scenario")
    parser.add_argument("--tracker-container", help="Docker tracker restarted before every scenario")
    parser.add_argument("--target-container", help="Docker target restarted before every scenario")
    parser.add_argument("--insecure", action="store_true", help="disable certificate verification for local TLS probes")
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    variables = {key: str(value) for key, value in manifest.get("defaults", {}).items()}
    variables["torrent_link"] = args.torrent_link or variables["torrent_link"]
    variables["torrent_hash"] = args.torrent_hash or variables["torrent_hash"]
    if args.torrent_file:
        variables["torrent_file"] = str(args.torrent_file.resolve())
    if args.basic_auth:
        variables["basic_auth"] = args.basic_auth
    context = ssl._create_unverified_context() if args.insecure else None
    selected = set(args.only.split(",")) if args.only else None
    scenarios = [
        scenario
        for scenario in manifest["scenarios"]
        if args.profile in scenario.get("profiles", ["direct"])
        and (selected is None or scenario["id"] in selected)
    ]

    cases: list[dict[str, Any]] = []
    lifecycle: list[dict[str, Any]] = []
    failures: list[str] = []
    global_setup, global_setup_failures = run_steps(
        args.base_url,
        manifest.get("global_setup", []),
        variables,
        args.timeout,
        args.readiness_timeout,
        context,
    )
    lifecycle.append({"scenario": "*", "phase": "setup", "steps": [summary(item) for item in global_setup]})
    failures.extend(f"global setup: {failure}" for failure in global_setup_failures)
    for scenario in scenarios:
        if args.target_container:
            target_reset = restart_target(
                args.docker_socket,
                args.target_container,
                args.base_url,
                variables,
                min(args.readiness_timeout, 30),
                context,
            )
            lifecycle.append({"scenario": scenario["id"], "phase": "target-reset", "steps": [target_reset]})
            if target_reset["status"] is None:
                failures.append(f"{scenario['id']} target reset: {target_reset['error']}")
        if args.tracker_container:
            if not args.seeder_rpc or not args.seeder_container:
                parser.error("--tracker-container requires --seeder-rpc and --seeder-container")
            fixture_reset = reset_tracker(
                args.docker_socket,
                args.tracker_container,
                args.seeder_container,
                args.seeder_rpc,
                min(args.readiness_timeout, 30),
            )
            lifecycle.append({"scenario": scenario["id"], "phase": "fixture-reset", "steps": [fixture_reset]})
            if fixture_reset["status"] is None:
                failures.append(f"{scenario['id']} fixture reset: {fixture_reset['error']}")
        elif args.seeder_container:
            if not args.seeder_rpc:
                parser.error("--seeder-container requires --seeder-rpc")
            fixture_reset = restart_seeder(
                args.docker_socket,
                args.seeder_container,
                args.seeder_rpc,
                min(args.readiness_timeout, 30),
            )
            lifecycle.append({"scenario": scenario["id"], "phase": "fixture-reset", "steps": [fixture_reset]})
            if fixture_reset["status"] is None:
                failures.append(f"{scenario['id']} fixture reset: {fixture_reset['error']}")
        elif args.seeder_rpc:
            fixture_reset = reset_seeder(args.seeder_rpc, min(args.timeout, 10))
            lifecycle.append({"scenario": scenario["id"], "phase": "fixture-reset", "steps": [fixture_reset]})
            if fixture_reset["status"] is None:
                failures.append(f"{scenario['id']} fixture reset: {fixture_reset['error']}")
        setup_steps = [*manifest.get("default_setup", []), *scenario.get("setup", [])]
        setup_results, setup_failures = run_steps(
            args.base_url, setup_steps, variables, args.timeout, args.readiness_timeout, context
        )
        lifecycle.append({"scenario": scenario["id"], "phase": "setup", "steps": [summary(item) for item in setup_results]})
        failures.extend(f"{scenario['id']} setup: {failure}" for failure in setup_failures)

        case = request(args.base_url, scenario, variables, args.timeout, context)
        cases.append(case)
        case_failure = check_step(case, scenario)
        if case_failure:
            failures.append(case_failure)

        teardown_steps = [*scenario.get("teardown", []), *manifest.get("default_teardown", [])]
        teardown_results, teardown_failures = run_steps(
            args.base_url, teardown_steps, variables, args.timeout, args.readiness_timeout, context
        )
        lifecycle.append({"scenario": scenario["id"], "phase": "teardown", "steps": [summary(item) for item in teardown_results]})
        failures.extend(f"{scenario['id']} teardown: {failure}" for failure in teardown_failures)

    global_teardown, global_teardown_failures = run_steps(
        args.base_url,
        manifest.get("global_teardown", []),
        variables,
        args.timeout,
        args.readiness_timeout,
        context,
    )
    lifecycle.append({"scenario": "*", "phase": "teardown", "steps": [summary(item) for item in global_teardown]})
    failures.extend(f"global teardown: {failure}" for failure in global_teardown_failures)

    result = {
        "schema": "rustorr.r6.contract-corpus.v2",
        "valid": not failures,
        "reference": manifest["reference"],
        "manifest": str(args.manifest),
        "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
        "profile": args.profile,
        "normalization": manifest.get("normalization", {}),
        "lifecycle": lifecycle,
        "failures": failures,
        "cases": cases,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(args.output)
    if failures:
        for failure in failures:
            print(f"error: {failure}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
