#!/usr/bin/env python3
"""Checks the workspace crate boundaries from docs/r4-plan.md.

Reads `cargo metadata --no-deps` JSON on stdin. Every workspace crate must be
listed in ALLOWED with the workspace crates it may depend on, and third-party
crates with a single owner may appear only in that owner. A new crate or a new
edge fails the check until this table is changed on purpose.
"""
import json
import sys

ALLOWED = {
    "rustorr-domain": set(),
    "rustorr-engine": {"rustorr-domain", "rustorr-cache"},
    "rustorr-cache": {"rustorr-domain"},
    "rustorr-state": {"rustorr-domain"},
    "rustorr-lifecycle": {"rustorr-domain", "rustorr-engine", "rustorr-cache", "rustorr-state"},
    "rustorr-search": set(),
    "rustorr-discovery": set(),
    "rustorr-gstreamer": set(),
    "rustorr-vfs": {"rustorr-lifecycle"},
    "rustorr-fuse": {"rustorr-lifecycle", "rustorr-vfs"},
    "rustorr-http": {"rustorr-lifecycle", "rustorr-search", "rustorr-vfs", "rustorr-gstreamer"},
    "rustorr-server": {
        "rustorr-domain",
        "rustorr-engine",
        "rustorr-cache",
        "rustorr-state",
        "rustorr-http",
        "rustorr-lifecycle",
        "rustorr-search",
        "rustorr-discovery",
        "rustorr-vfs",
        "rustorr-fuse",
        "rustorr-gstreamer",
    },
}

# Dependency name prefix -> the crates allowed to depend on it. librqbit is
# ADR 0003. anyhow is in rustorr-engine only because librqbit's storage traits
# return it, and in the binary, the one place that reports errors to a human;
# other libraries define their own error types.
EXTERNAL_OWNERS = {
    "librqbit": {"rustorr-engine"},
    "anyhow": {"rustorr-engine", "rustorr-server"},
    "axum": {"rustorr-http"},
    "tower": {"rustorr-http"},
    "rusqlite": {"rustorr-state"},
    "gstreamer": {"rustorr-gstreamer"},
    "libsqlite3-sys": {"rustorr-state"},
}


def main() -> int:
    packages = json.load(sys.stdin)["packages"]
    workspace = {package["name"] for package in packages}
    errors = []

    for name in sorted(workspace - ALLOWED.keys()):
        errors.append(f"{name}: not in the boundary table; add it to ALLOWED in {sys.argv[0]}")

    for package in packages:
        name = package["name"]
        if name not in ALLOWED:
            continue
        for dependency in package["dependencies"]:
            target = dependency["name"]
            if target in workspace and target not in ALLOWED[name]:
                errors.append(f"{name} -> {target}: workspace edge is not allowed")
            for prefix, owners in EXTERNAL_OWNERS.items():
                if target.startswith(prefix) and name not in owners:
                    allowed = " or ".join(sorted(owners))
                    errors.append(f"{name} -> {target}: only {allowed} may depend on {prefix}*")

    for error in errors:
        print(f"boundary violation: {error}", file=sys.stderr)
    if not errors:
        print(f"boundaries ok: {len(workspace)} crates")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
