#!/usr/bin/env python3
"""Aggregate raw R1 measurement files without hiding scenario-specific failures."""

from __future__ import annotations

import argparse
import json
import pathlib
import statistics


def values(node: object, key: str) -> list[float]:
    found: list[float] = []
    if isinstance(node, dict):
        value = node.get(key)
        if isinstance(value, (int, float)):
            found.append(float(value))
        for child in node.values():
            found.extend(values(child, key))
    elif isinstance(node, list):
        for child in node:
            found.extend(values(child, key))
    return found


def count(node: object, key: str, expected: object) -> int:
    if isinstance(node, dict):
        total = int(node.get(key) == expected)
        return total + sum(count(child, key, expected) for child in node.values())
    if isinstance(node, list):
        return sum(count(child, key, expected) for child in node)
    return 0


def percentile(items: list[float], fraction: float) -> float | None:
    if not items:
        return None
    ordered = sorted(items)
    return ordered[min(len(ordered) - 1, round((len(ordered) - 1) * fraction))]


def metric_summary(items: list[float]) -> dict:
    return {"count": len(items), "p50": percentile(items, .5), "p95": percentile(items, .95), "mean": statistics.mean(items) if items else None}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("measurements", nargs="+", type=pathlib.Path)
    args = parser.parse_args()

    runs = [json.loads(path.read_text(encoding="utf-8")) for path in args.measurements]
    scenario_ids = [scenario["id"] for scenario in runs[0]["scenarios"]["scenarios"]]
    by_scenario = {}
    for scenario_id in scenario_ids:
        events = [event for run in runs for event in run.get("events", []) if event.get("scenario") == scenario_id]
        by_scenario[scenario_id] = {
            "runs": len([run for run in runs if any(event.get("scenario") == scenario_id for event in run.get("events", []))]),
            "status": sorted({str(event.get("status")) for event in events if event.get("status")}),
            "client_start_ms": metric_summary([value for event in events for value in values(event, "client_start_ms")]),
            "metadata_discovery_ms": metric_summary([value for event in events for value in values(event, "metadata_discovery_ms")]),
            "seek_ms": metric_summary([value for event in events for value in values(event, "seek_ms")]),
            "recovery_ms": metric_summary([value for event in events for value in values(event, "recovery_ms")]),
            "latency_ms": metric_summary([value for event in events for value in values(event, "latency_ms")]),
            "failed_requests": sum(count(event, "ok", False) for event in events),
            "stalls": sum(count(event, "stall", True) for event in events) + sum(int(event.get("stalls", 0)) for event in events),
        }

    result = {
        "schema": "rustorr.r1.baseline.v1",
        "reference": runs[0].get("reference"),
        "runs": [str(path) for path in args.measurements],
        "method": {
            "stall_threshold_ms": 2000,
            "stall_definition": "request failure or request latency >= threshold; this is a transport proxy, not decoded-player rebuffering",
            "noise": [
                "Docker Desktop scheduling and shared host CPU/storage",
                "piece availability and peer handshake timing",
                "cold Range latency includes cache/piece acquisition",
                "peer departure can remain stall-free when the requested range is already cached",
            ],
        },
        "scenarios": by_scenario,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
