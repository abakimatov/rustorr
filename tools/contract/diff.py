#!/usr/bin/env python3
"""Compare R6 corpora and enforce the declared deferred-route boundary."""

from __future__ import annotations

import argparse
import base64
import copy
import hashlib
import json
import pathlib
import re
from email.utils import parsedate_to_datetime
from typing import Any


def normalize_headers(headers: dict[str, str], rules: dict[str, str]) -> dict[str, str]:
    normalized = {key.lower(): value for key, value in headers.items()}
    for key, rule in rules.items():
        key = key.lower()
        if key not in normalized:
            continue
        if rule == "ignore":
            normalized.pop(key)
        elif rule == "http-date":
            try:
                parsedate_to_datetime(normalized[key])
            except (TypeError, ValueError):
                continue
            normalized[key] = "<http-date>"
        else:
            raise ValueError(f"unknown header normalization {rule!r} for {key}")
    return normalized


def remove_path(value: Any, components: list[str]) -> bool:
    if not components:
        return False
    head, *tail = components
    changed = False
    if isinstance(value, dict):
        keys = list(value) if head == "*" else [head]
        for key in keys:
            if key not in value:
                continue
            if tail:
                changed = remove_path(value[key], tail) or changed
            else:
                value.pop(key)
                changed = True
    elif isinstance(value, list):
        indexes = range(len(value)) if head == "*" else [int(head)] if head.isdigit() else []
        for index in indexes:
            if index >= len(value):
                continue
            if tail:
                changed = remove_path(value[index], tail) or changed
            else:
                value[index] = None
                changed = True
    return changed


def normalize_json(value: Any, paths: list[str]) -> tuple[Any, bool]:
    value = copy.deepcopy(value)
    changed = False
    for path in paths:
        if not path.startswith("/"):
            raise ValueError(f"normalization path must be an absolute JSON pointer: {path}")
        components = [component.replace("~1", "/").replace("~0", "~") for component in path[1:].split("/")]
        changed = remove_path(value, components) or changed
    return value, changed


def comparable(case: dict[str, Any], normalization: dict[str, Any]) -> dict[str, Any]:
    response = case["response"]
    headers = normalize_headers(response.get("headers", {}), normalization.get("headers", {}))
    json_paths = normalization.get("json_paths", [])
    semantic_json, _ = normalize_json(response.get("json"), json_paths)
    # JSON is compared structurally. Content-Length is only a derived encoding
    # detail and an optional dynamic field may be present on one side only, so
    # normalize it whenever this manifest declares dynamic JSON paths.
    if semantic_json is not None and json_paths:
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
            body = body.replace(token, b"<multipart-boundary>")
            headers["content-type"] = content_type.replace(boundary.group(1), "<multipart-boundary>")
        result["body_sha256"] = hashlib.sha256(body).hexdigest()
        result["body_bytes"] = len(body)
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("reference", type=pathlib.Path)
    parser.add_argument("candidate", type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument(
        "--allowlist",
        type=pathlib.Path,
        default=pathlib.Path(__file__).with_name("deferred-routes.json"),
    )
    args = parser.parse_args()

    reference = json.loads(args.reference.read_text(encoding="utf-8"))
    candidate = json.loads(args.candidate.read_text(encoding="utf-8"))
    allowlist = json.loads(args.allowlist.read_text(encoding="utf-8"))
    if not reference.get("valid") or not candidate.get("valid"):
        raise SystemExit("both captures must be valid (no transport errors or null statuses)")
    if reference.get("profile") != candidate.get("profile"):
        raise SystemExit("capture profiles differ")
    if reference.get("manifest_sha256") != candidate.get("manifest_sha256"):
        raise SystemExit("captures were produced from different manifests")
    normalization = reference.get("normalization", {})
    if normalization != candidate.get("normalization", {}):
        raise SystemExit("capture normalization declarations differ")

    left = {case["id"]: comparable(case, normalization) for case in reference["cases"]}
    right = {case["id"]: comparable(case, normalization) for case in candidate["cases"]}
    differences = []
    for case_id in sorted(set(left) | set(right)):
        if case_id not in left or case_id not in right:
            differences.append(
                {
                    "id": case_id,
                    "kind": "missing-case",
                    "reference": case_id in left,
                    "candidate": case_id in right,
                }
            )
        elif left[case_id] != right[case_id]:
            differences.append(
                {
                    "id": case_id,
                    "kind": "response-difference",
                    "reference": left[case_id],
                    "candidate": right[case_id],
                }
            )

    declared = {entry["scenario"]: entry for entry in allowlist["entries"]}
    # Each profile captures its own subset of the manifest. A deferred route
    # that the profile never requested cannot be expected to differ in it.
    captured = set(left) | set(right)
    allowed = {
        scenario: entry
        for scenario, entry in declared.items()
        if entry.get("expected_difference", True) and scenario in captured
    }
    observed = {difference["id"] for difference in differences}
    core = [difference for difference in differences if difference["id"] not in allowed]
    deferred = [
        {**difference, "allowlist": allowed[difference["id"]]}
        for difference in differences
        if difference["id"] in allowed
    ]
    missing_deferred = [allowed[case_id] for case_id in sorted(set(allowed) - observed)]
    passed = not core and not missing_deferred
    result = {
        "schema": "rustorr.r6.contract-diff.v2",
        "profile": reference["profile"],
        "normalization": normalization,
        "equal_core": not core,
        "deferred_matches_allowlist": not missing_deferred,
        "passed": passed,
        "difference_count": len(differences),
        "core_difference_count": len(core),
        "deferred_difference_count": len(deferred),
        "core_differences": core,
        "deferred_differences": deferred,
        "missing_deferred_differences": missing_deferred,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "passed": passed,
                "core_difference_count": len(core),
                "deferred_difference_count": len(deferred),
                "missing_deferred_difference_count": len(missing_deferred),
            },
            sort_keys=True,
        )
    )
    for difference in core:
        print(f"- core {difference['id']}: {difference['kind']}")
    for entry in missing_deferred:
        print(f"- deferred allowlist entry did not differ: {entry['scenario']}")
    raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
