#!/usr/bin/env python3
"""Capture a comparable TCP/netem profile for one warmed HTTP Range path.

This is deliberately separate from ``measure.py``.  It is a diagnostic aid,
not a new R1 gate and not part of Rustorr's HTTP contract.  The target has
already been prepared and warmed before netem is installed; each result then
contains the timed Range result plus transport observations made outside that
timed request.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import select
import statistics
import subprocess
import time
from typing import Any

import measure


SAMPLES = 20
DELAY_MS = 80
LOSS_PERCENT = 1
TCP_SEPARATOR = "\x1e"


def qdisc_counters(result: dict[str, Any]) -> dict[str, int] | None:
    """Extract the cumulative counters emitted by ``tc -s qdisc``."""
    if not result.get("ok"):
        return None
    match = re.search(
        r"Sent\s+(?P<bytes>\d+)\s+bytes\s+(?P<packets>\d+)\s+pkt\s+"
        r"\(dropped\s+(?P<dropped>\d+),\s+overlimits\s+(?P<overlimits>\d+),?\s+"
        r"requeues\s+(?P<requeues>\d+)\)",
        result.get("stdout", ""),
    )
    return {key: int(value) for key, value in match.groupdict().items()} if match else None


def qdisc_snapshot(container: str, image: str) -> dict[str, Any]:
    result = measure.run_netem(container, image, "-s", "qdisc", "show", "dev", "eth0")
    return {"command": result, "counters": qdisc_counters(result)}


def network_command(container: str, image: str, *command: str) -> dict[str, Any]:
    try:
        result = subprocess.run(
            ["docker", "run", "--rm", "--network", f"container:{container}", image, *command],
            capture_output=True,
            text=True,
            check=False,
            timeout=35,
        )
        return {"ok": result.returncode == 0, "returncode": result.returncode, "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}
    except subprocess.TimeoutExpired as error:
        return {"ok": False, "error": f"network helper timed out: {error}"}


def link_counters(result: dict[str, Any]) -> dict[str, int] | None:
    if not result.get("ok"):
        return None
    counters: dict[str, int] = {}
    for direction in ("RX", "TX"):
        match = re.search(
            rf"{direction}:\s+bytes\s+packets\s+errors\s+dropped[^\n]*\n\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)",
            result.get("stdout", ""),
        )
        if match is None:
            return None
        for name, value in zip(("bytes", "packets", "errors", "dropped"), match.groups()):
            counters[f"{direction.lower()}_{name}"] = int(value)
    return counters


def link_snapshot(container: str, image: str) -> dict[str, Any]:
    result = network_command(container, image, "ip", "-s", "link", "show", "dev", "eth0")
    return {"command": result, "counters": link_counters(result)}


def wait_for_quiescence(container: str, image: str, timeout_s: float = 20) -> dict[str, Any]:
    """Wait until warm-up and its prefetch have stopped moving target traffic."""
    started = time.monotonic()
    previous = link_snapshot(container, image)
    observations = [previous]
    stable = 0
    while time.monotonic() - started < timeout_s:
        time.sleep(0.25)
        current = link_snapshot(container, image)
        observations.append(current)
        delta = counter_delta(previous["counters"], current["counters"])
        if delta is not None and delta["rx_bytes"] == 0 and delta["tx_bytes"] == 0:
            stable += 1
            if stable == 2:
                return {"ok": True, "elapsed_ms": (time.monotonic() - started) * 1000, "observations": observations}
        else:
            stable = 0
        previous = current
    return {"ok": False, "elapsed_ms": (time.monotonic() - started) * 1000, "observations": observations}


def counter_delta(before: dict[str, int] | None, after: dict[str, int] | None) -> dict[str, int] | None:
    if before is None or after is None:
        return None
    return {key: after[key] - before[key] for key in before}


def tcp_monitor_command(container: str, image: str) -> list[str]:
    return [
        "docker",
        "run",
        "--rm",
        "--network",
        f"container:{container}",
        image,
        "sh",
        "-c",
        "printf 'READY\\n'; while :; do date +%s%N; ss -tinH 'sport = :8090'; printf '\\036\\n'; sleep 0.02; done",
    ]


def parse_tcp_samples(stdout: str) -> list[dict[str, Any]]:
    """Summarise every ``ss -tin`` observation made while curl was active."""
    samples: list[dict[str, Any]] = []
    for observation in stdout.split(TCP_SEPARATOR):
        lines = [line.strip() for line in observation.splitlines() if line.strip()]
        if len(lines) < 2 or not lines[0].isdigit():
            continue
        details = " ".join(lines[1:])
        if "rtt:" not in details and "cwnd:" not in details:
            continue
        item: dict[str, Any] = {"observed_at_ns": int(lines[0])}
        rtt = re.search(r"rtt:([\d.]+)/([\d.]+)", details)
        if rtt:
            item["rtt_ms"] = float(rtt.group(1))
            item["rtt_variance_ms"] = float(rtt.group(2))
        for name, pattern in {
            "cwnd": r"cwnd:(\d+)",
            "bytes_retrans": r"bytes_retrans:(\d+)",
            "unacked": r"unacked:(\d+)",
        }.items():
            found = re.search(pattern, details)
            if found:
                item[name] = int(found.group(1))
        retrans = re.search(r"(?:^|\s)retrans:(\d+)(?:/(\d+))?", details)
        if retrans:
            item["retrans"] = int(retrans.group(1))
            if retrans.group(2) is not None:
                item["retrans_total"] = int(retrans.group(2))
        samples.append(item)
    return samples


def wait_for_tcp_monitor(monitor: subprocess.Popen[str], timeout_s: float = 5) -> bool:
    if monitor.stdout is None:
        return False
    readable, _, _ = select.select([monitor.stdout], [], [], timeout_s)
    return bool(readable) and monitor.stdout.readline().strip() == "READY"


def capture_sample(base_url: str, link: str, container: str, image: str) -> dict[str, Any]:
    """Observe one Range request without including observer work in latency."""
    before = qdisc_snapshot(container, image)
    monitor = subprocess.Popen(
        tcp_monitor_command(container, image),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    # Starting and confirming the monitor is deliberately outside Range
    # timing. A sample without TCP state is diagnostic failure, not evidence.
    monitor_ready = wait_for_tcp_monitor(monitor)
    try:
        range_result = (
            measure.range_request(base_url, link, measure.FILE_SIZE // 2)
            if monitor_ready
            else {"ok": False, "error": "TCP monitor did not become ready", "latency_ms": None, "stall": True}
        )
    finally:
        monitor.terminate()
        try:
            stdout, stderr = monitor.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            monitor.kill()
            stdout, stderr = monitor.communicate(timeout=5)
    after = qdisc_snapshot(container, image)
    return {
        "range": range_result,
        "qdisc_before": before,
        "qdisc_after": after,
        "qdisc_delta": counter_delta(before["counters"], after["counters"]),
        "tcp": {"ready": monitor_ready, "samples": parse_tcp_samples(stdout), "monitor_stderr": stderr.strip()},
    }


def latency_summary(samples: list[dict[str, Any]]) -> dict[str, float | int | None]:
    latencies = sorted(
        sample["range"]["latency_ms"]
        for sample in samples
        if isinstance(sample.get("range", {}).get("latency_ms"), (int, float))
    )
    if not latencies:
        return {"count": 0, "p50": None, "p95": None, "mean": None}
    return {
        "count": len(latencies),
        "p50": latencies[round((len(latencies) - 1) * 0.5)],
        "p95": latencies[round((len(latencies) - 1) * 0.95)],
        "mean": statistics.mean(latencies),
    }


def artifact_errors(result: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    if not result.get("quiescence", {}).get("ok"):
        errors.append("target did not become network-idle after warm-up")
    if len(result.get("samples", [])) != SAMPLES:
        errors.append(f"expected {SAMPLES} samples")
    for index, sample in enumerate(result.get("samples", [])):
        response = sample.get("range", {})
        if not response.get("ok"):
            errors.append(f"sample {index}: invalid Range response")
        if sample.get("qdisc_delta") is None:
            errors.append(f"sample {index}: missing qdisc delta")
        if not sample.get("tcp", {}).get("samples"):
            errors.append(f"sample {index}: missing TCP observation")
    return errors


def wait_for_api(base_url: str, timeout_s: float = 30) -> dict[str, Any]:
    """Wait for the target's torrent API, not merely its listening socket."""
    started = time.monotonic()
    last: dict[str, Any] | None = None
    while time.monotonic() - started < timeout_s:
        last = measure.post_json(base_url, "/torrents", {"action": "list"}, timeout=2)
        if last.get("ok"):
            return {"ok": True, "elapsed_ms": (time.monotonic() - started) * 1000, "last": last}
        time.sleep(0.25)
    return {"ok": False, "elapsed_ms": (time.monotonic() - started) * 1000, "last": last}


def run(args: argparse.Namespace) -> dict[str, Any]:
    result: dict[str, Any] = {
        "schema": "rustorr.r1.netem-diagnosis.v1",
        "target": args.target,
        "network": {"delay_ms": DELAY_MS, "loss_percent": LOSS_PERCENT},
        "readiness": None,
        "quiescence": None,
        "prepared": [],
        "samples": [],
        "netem_apply": None,
        "netem_clear": None,
        "summary": None,
        "complete": False,
        "stop_reason": None,
    }
    measure.atomic_write(args.output, result)
    result["readiness"] = wait_for_api(args.base_url)
    if not result["readiness"].get("ok"):
        result["stop_reason"] = "API readiness failed"
        measure.atomic_write(args.output, result)
        return result
    result["prepared"] = measure.prepare(args.base_url, args.known_link, args.known_hash)
    if any(not event.get("ok") for event in result["prepared"]):
        result["stop_reason"] = "prepare failed"
        measure.atomic_write(args.output, result)
        return result
    # Both reads are preconditions, not samples: they make the measured middle
    # Range resident on both targets before loss is introduced.
    for offset in (0, measure.FILE_SIZE // 2):
        warm = measure.range_request(args.base_url, args.known_link, offset)
        result["prepared"].append({"scenario": "warm-precondition", **warm})
        if not warm.get("ok"):
            result["stop_reason"] = "warm precondition failed"
            measure.atomic_write(args.output, result)
            return result
    result["quiescence"] = wait_for_quiescence(args.target_container, args.netem_image)
    if not result["quiescence"].get("ok"):
        result["stop_reason"] = "target did not become network-idle after warm-up"
        measure.atomic_write(args.output, result)
        return result
    result["netem_apply"] = measure.apply_netem(args.target_container, args.netem_image, DELAY_MS, LOSS_PERCENT)
    measure.atomic_write(args.output, result)
    if not result["netem_apply"].get("ok"):
        result["stop_reason"] = "netem apply failed"
        measure.atomic_write(args.output, result)
        return result
    try:
        for index in range(SAMPLES):
            sample = {"index": index, **capture_sample(args.base_url, args.known_link, args.target_container, args.netem_image)}
            result["samples"].append(sample)
            measure.atomic_write(args.output, result)
            if not sample["range"].get("ok"):
                result["stop_reason"] = f"sample {index}: invalid Range response"
                break
    finally:
        result["netem_clear"] = measure.clear_netem(args.target_container, args.netem_image)
    result["summary"] = {"latency_ms": latency_summary(result["samples"])}
    errors = artifact_errors(result)
    result["complete"] = not errors and result["netem_clear"].get("ok")
    if not result["complete"] and result["stop_reason"] is None:
        result["stop_reason"] = "; ".join(errors or ["netem clear failed"])
    measure.atomic_write(args.output, result)
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8090")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--known-link", required=True)
    parser.add_argument("--known-hash", required=True)
    parser.add_argument("--target", required=True, choices=("torrserver", "rustorr"))
    parser.add_argument("--target-container", required=True)
    parser.add_argument("--netem-image", default="rustorr-r1-fixture")
    args = parser.parse_args()
    result = run(args)
    if not result["complete"]:
        raise SystemExit(result["stop_reason"] or "incomplete netem diagnosis")


if __name__ == "__main__":
    main()
