#!/usr/bin/env python3
"""Prove a Range survives a Rustorr restart with no seeder available."""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import time

from measure import atomic_write, http_request, prepare, range_request


def docker(*arguments: str, timeout: int = 45) -> dict:
    try:
        result = subprocess.run(
            ["docker", *arguments],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
        return {
            "ok": result.returncode == 0,
            "returncode": result.returncode,
            "stdout": result.stdout.strip(),
            "stderr": result.stderr.strip(),
        }
    except subprocess.TimeoutExpired as error:
        return {"ok": False, "error": str(error)}


def require(result: dict, label: str) -> None:
    if not result.get("ok"):
        raise RuntimeError(f"{label} failed: {result}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8090")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--known-link", required=True)
    parser.add_argument("--known-hash", required=True)
    parser.add_argument("--rustorr-container", required=True)
    parser.add_argument("--seeder-container", required=True)
    args = parser.parse_args()

    result = {
        "schema": "rustorr.r5.restart-probe.v1",
        "complete": False,
        "stop_reason": None,
        "events": [],
    }
    atomic_write(args.output, result)
    try:
        prepared = prepare(
            args.base_url,
            args.known_link,
            args.known_hash,
            save_to_db=True,
        )
        result["events"].extend(prepared)
        for event in prepared:
            require(event, event["scenario"])

        first = range_request(args.base_url, args.known_link, 0)
        result["events"].append({"scenario": "before-restart", **first})
        require(first, "before-restart Range")
        atomic_write(args.output, result)

        restarted = docker("restart", "--timeout", "10", args.rustorr_container)
        result["events"].append({"scenario": "restart", **restarted})
        require(restarted, "Rustorr restart")
        ready = None
        for _ in range(60):
            ready = http_request(args.base_url, "/echo", timeout=2)
            if ready.get("ok"):
                break
            time.sleep(0.5)
        ready = {"scenario": "restart-readiness", **(ready or {"ok": False, "error": "no probe"})}
        result["events"].append(ready)
        require(ready, "restart readiness")

        stopped = docker("stop", "--timeout", "10", args.seeder_container)
        result["events"].append({"scenario": "seeder-stop", **stopped})
        require(stopped, "seeder stop")

        second = range_request(args.base_url, args.known_link, 0)
        result["events"].append({"scenario": "after-restart-without-peer", **second})
        require(second, "after-restart Range")
        if first["sha256"] != second["sha256"]:
            raise RuntimeError("Range digest changed across restart")

        result["same_digest"] = True
        result["complete"] = True
        atomic_write(args.output, result)
    except KeyboardInterrupt:
        result["stop_reason"] = "interrupted"
        atomic_write(args.output, result)
        raise
    except Exception as error:
        result["stop_reason"] = f"{type(error).__name__}: {error}"
        atomic_write(args.output, result)
        raise


if __name__ == "__main__":
    main()
