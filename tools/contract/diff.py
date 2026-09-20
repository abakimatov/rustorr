#!/usr/bin/env python3
"""Compare two R2 corpus files without hiding contract-relevant differences."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import pathlib
import re
from typing import Any


def without_ignored_headers(headers: dict[str, str], ignored: set[str]) -> dict[str, str]:
    return {key.lower(): value for key, value in headers.items() if key.lower() not in ignored}


def without_ignored_json_keys(value: Any, ignored: set[str]) -> Any:
    if isinstance(value, dict):
        return {key: without_ignored_json_keys(item, ignored) for key, item in value.items() if key not in ignored}
    if isinstance(value, list):
        return [without_ignored_json_keys(item, ignored) for item in value]
    return value


def comparable(case: dict[str, Any], ignored_headers: set[str], ignored_json_keys: set[str]) -> dict[str, Any]:
    response = case["response"]
    semantic_json = without_ignored_json_keys(response.get("json"), ignored_json_keys)
    headers = without_ignored_headers(response.get("headers", {}), ignored_headers)
    if semantic_json is not None and ignored_json_keys:
        # Content-Length is derived from the raw JSON representation; ignored
        # dynamic fields may change it while the semantic document is equal.
        headers.pop("content-length", None)
    result = {
        "id": case["id"],
        "status": response["status"],
        "headers": headers,
        "json": semantic_json,
    }
    if semantic_json is None:
        body = base64.b64decode(response["body_base64"])
        content_type = headers.get("content-type", "")
        boundary = re.search(r"boundary=([^;]+)", content_type)
        if boundary:
            token = boundary.group(1).encode()
            body = body.replace(token, b"<normalized-boundary>")
            headers["content-type"] = content_type.replace(boundary.group(1), "<normalized-boundary>")
        result["body_sha256"] = hashlib.sha256(body).hexdigest()
        result["body_bytes"] = response["body_bytes"]
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("reference", type=pathlib.Path)
    parser.add_argument("candidate", type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args()
    reference = json.loads(args.reference.read_text(encoding="utf-8"))
    candidate = json.loads(args.candidate.read_text(encoding="utf-8"))
    ignored = set(reference.get("normalization", {}).get("ignored_headers", []))
    ignored_json = set(reference.get("normalization", {}).get("ignored_json_paths", []))
    left = {case["id"]: comparable(case, ignored, ignored_json) for case in reference["cases"]}
    right = {case["id"]: comparable(case, ignored, ignored_json) for case in candidate["cases"]}
    differences = []
    for case_id in sorted(set(left) | set(right)):
        if case_id not in left or case_id not in right:
            differences.append({"id": case_id, "kind": "missing-case", "reference": case_id in left, "candidate": case_id in right})
        elif left[case_id] != right[case_id]:
            differences.append({"id": case_id, "kind": "response-difference", "reference": left[case_id], "candidate": right[case_id]})
    result = {"schema": "rustorr.r2.contract-diff.v1", "ignored_headers": sorted(ignored), "ignored_json_keys": sorted(ignored_json), "equal": not differences, "difference_count": len(differences), "differences": differences}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"equal": result["equal"], "difference_count": result["difference_count"]}, sort_keys=True))
    for difference in differences:
        print(f"- {difference['id']}: {difference['kind']}")
    raise SystemExit(0 if result["equal"] else 1)


if __name__ == "__main__":
    main()
