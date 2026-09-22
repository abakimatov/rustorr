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


def values_of_lists(node: object, key: str) -> list[list]:
    found: list[list] = []
    if isinstance(node, dict):
        if isinstance(node.get(key), list):
            found.append(node[key])
        for child in node.values():
            found.extend(values_of_lists(child, key))
    elif isinstance(node, list):
        for child in node:
            found.extend(values_of_lists(child, key))
    return found


def stall_count(event: dict) -> int:
    aggregate = event.get("stalls")
    if isinstance(aggregate, int):
        return aggregate
    return count(event, "stall", True)


def control_errors(events: object) -> int:
    if isinstance(events, dict):
        total = int(events.get("scenario") == "eviction-control" and events.get("ok") is False)
        for key in ("netem_apply", "netem_clear", "seeder_stop"):
            value = events.get(key)
            total += int(isinstance(value, dict) and value.get("ok") is False)
        return total + sum(control_errors(child) for child in events.values())
    if isinstance(events, list):
        return sum(control_errors(child) for child in events)
    return 0


def within(summary: dict, maximum: float) -> bool:
    value = summary.get("p95")
    return isinstance(value, (int, float)) and value <= maximum


def percentile(items: list[float], fraction: float) -> float | None:
    if not items:
        return None
    ordered = sorted(items)
    return ordered[min(len(ordered) - 1, round((len(ordered) - 1) * fraction))]


def metric_summary(items: list[float]) -> dict:
    return {"count": len(items), "p50": percentile(items, .5), "p95": percentile(items, .95), "mean": statistics.mean(items) if items else None}


def metric_summaries_by_run(runs: list[dict], scenario_id: str, key: str) -> list[dict]:
    summaries = []
    for run in runs:
        events = [event for event in run.get("events", []) if event.get("scenario") == scenario_id]
        summaries.append(metric_summary([value for event in events for value in values(event, key)]))
    return summaries


def every_run_within(summaries: list[dict], maximum: float) -> bool:
    return bool(summaries) and all(within(summary, maximum) for summary in summaries)


def netem_sample_counts(runs: list[dict]) -> list[int]:
    return [
        sum(
            len(event.get("ranges", []))
            for event in run.get("events", [])
            if event.get("scenario") == "netem-delay-loss"
        )
        for run in runs
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--targets", type=pathlib.Path)
    parser.add_argument("measurements", nargs="+", type=pathlib.Path)
    args = parser.parse_args()

    runs = [json.loads(path.read_text(encoding="utf-8")) for path in args.measurements]
    if args.targets:
        incomplete = [str(path) for path, run in zip(args.measurements, runs) if not run.get("complete")]
        if incomplete:
            raise SystemExit(f"incomplete measurements cannot close the gate: {incomplete}")
    scenario_ids = [scenario["id"] for scenario in runs[0]["scenarios"]["scenarios"]]
    by_scenario = {}
    for scenario_id in scenario_ids:
        events = [event for run in runs for event in run.get("events", []) if event.get("scenario") == scenario_id]
        by_scenario[scenario_id] = {
            "runs": len([run for run in runs if any(event.get("scenario") == scenario_id for event in run.get("events", []))]),
            "status": sorted({str(event.get("status")) for event in events if event.get("status")}),
            "client_start_ms": metric_summary([value for event in events for value in values(event, "client_start_ms")]),
            "metadata_discovery_ms": metric_summary([value for event in events for value in values(event, "metadata_discovery_ms")]),
            "magnet_first_range_ms": metric_summary([value for event in events for value in values(event, "magnet_first_range_ms")]),
            "seek_ms": metric_summary([value for event in events for value in values(event, "seek_ms")]),
            "recovery_ms": metric_summary([value for event in events for value in values(event, "recovery_ms")]),
            "latency_ms": metric_summary([value for event in events for value in values(event, "latency_ms")]),
            "latency_ms_by_run": metric_summaries_by_run(runs, scenario_id, "latency_ms"),
            "failed_requests": sum(count(event, "ok", False) for event in events),
            "stalls": sum(stall_count(event) for event in events),
        }

    errors = {
        "request": sum(
            count(run.get("events", []), "ok", False)
            for run in runs
        ),
        "integrity": sum(
            len(value)
            for run in runs
            for value in values_of_lists(run.get("events", []), "integrity_errors")
        ),
        "precondition": sum(
            count(run.get("events", []), "status", "precondition-failed")
            + count(run.get("events", []), "status", "blocked")
            for run in runs
        ),
        "control": sum(control_errors(run.get("events", [])) for run in runs),
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
        "errors": errors,
    }
    if args.targets:
        targets = json.loads(args.targets.read_text(encoding="utf-8"))["rustorr_targets"]
        required_netem_samples = targets["netem_range_p95_ms"].get("samples_per_run", 1)
        actual_netem_samples = netem_sample_counts(runs)
        checks = {
            "two_complete_runs": len(runs) >= 2,
            "all_scenarios_in_every_run": all(
                all(any(event.get("scenario") == scenario_id for event in run.get("events", [])) for scenario_id in scenario_ids)
                for run in runs
            ),
            "zero_request_errors": errors["request"] == 0,
            "zero_integrity_errors": errors["integrity"] == 0,
            "zero_precondition_errors": errors["precondition"] == 0,
            "zero_control_errors": errors["control"] == 0,
            "cold_known_torrent_floor": within(by_scenario["cold-known-torrent"]["client_start_ms"], targets["cold_known_torrent_p95_ms"]["max"]),
            "warm_known_torrent_floor": within(by_scenario["warm-known-torrent"]["client_start_ms"], targets["warm_known_torrent_p95_ms"]["max"]),
            "seek_loaded_floor": within(by_scenario["seek-loaded"]["seek_ms"], targets["seek_loaded_p95_ms"]["max"]),
            "netem_samples_per_run": all(count == required_netem_samples for count in actual_netem_samples),
            "netem_range_floor": every_run_within(by_scenario["netem-delay-loss"]["latency_ms_by_run"], targets["netem_range_p95_ms"]["max"]),
            "peer_departure": by_scenario["peer-departure"]["failed_requests"] == 0,
            "seek_missing_bounded": by_scenario["seek-missing"]["failed_requests"] == 0,
            "seek_evicted_bounded": by_scenario["seek-evicted"]["failed_requests"] == 0,
            "deterministic_eviction_control": all(
                any(event.get("scenario") == "eviction-control" and event.get("ok") for event in run.get("events", []))
                for run in runs
            ),
        }
        result["gate"] = {
            "passed": all(checks.values()),
            "checks": checks,
            "netem_latency_ms_by_run": by_scenario["netem-delay-loss"]["latency_ms_by_run"],
            "netem_sample_counts": actual_netem_samples,
            "seek_evicted_latency_ms": by_scenario["seek-evicted"]["seek_ms"],
            "note": "seek-evicted latency is reported without a parity floor",
        }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
