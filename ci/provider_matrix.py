#!/usr/bin/env python3
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT_DIR / "ci" / "provider-matrix.json"

VERSION_BUMP_PATHS = {"Cargo.toml", "Cargo.lock"}
VERSION_LINE_RE = re.compile(r'^version\s*=\s*"[^"]+"\s*$')


def load_matrix() -> dict:
    return json.loads(MATRIX_PATH.read_text())


def normalize_provider(raw: str, matrix: dict) -> str:
    value = raw.strip().lower()
    if value.startswith("messaging-"):
        value = value[len("messaging-") :]
    if value in matrix["providers"]:
        return value
    raise SystemExit(
        f"Unknown provider '{raw}'. Expected one of: "
        + ", ".join(sorted(matrix["providers"].keys()))
    )


def matches_path(path: str, candidate: str) -> bool:
    if candidate.endswith("/"):
        return path.startswith(candidate)
    return path == candidate


def detect_changed_files(base: str, head: str) -> list[str]:
    cmd = ["git", "diff", "--name-only", f"{base}..{head}"]
    output = subprocess.check_output(cmd, cwd=ROOT_DIR, text=True)
    return [line.strip() for line in output.splitlines() if line.strip()]


def is_version_only_diff(base: str, head: str, path: str) -> bool:
    """Return True if the diff for `path` only contains version-line changes.

    Handles both root Cargo.toml (single version line under [workspace.package])
    and Cargo.lock (many `version = "..."` lines for dependency crates).
    A pure version bump should not invalidate the provider-scope caching logic.
    """
    try:
        diff = subprocess.check_output(
            ["git", "diff", "-U0", f"{base}..{head}", "--", path],
            cwd=ROOT_DIR,
            text=True,
        )
    except subprocess.CalledProcessError:
        return False

    for line in diff.splitlines():
        if not line:
            continue
        if line.startswith(("diff ", "index ", "+++", "---", "@@")):
            continue
        if not line.startswith(("+", "-")):
            continue
        content = line[1:].strip()
        if not content:
            continue
        if VERSION_LINE_RE.match(content):
            continue
        return False
    return True


def shared_paths_are_version_only(
    base: str, head: str, shared_hits: list[str]
) -> bool:
    """All shared-path hits must be Cargo.toml/Cargo.lock AND version-only diffs."""
    if not shared_hits:
        return False
    for path in shared_hits:
        if path not in VERSION_BUMP_PATHS:
            return False
        if not is_version_only_diff(base, head, path):
            return False
    return True


def resolve_provider(args: argparse.Namespace) -> int:
    matrix = load_matrix()
    provider_name = normalize_provider(args.provider, matrix)
    provider = matrix["providers"][provider_name]
    result = {
      "provider": provider_name,
      "pack": provider["pack"],
      "components": provider["components"],
      "manifests": provider["manifests"]
    }
    print(json.dumps(result))
    return 0


def build_all_result(
    matrix: dict,
    changed_files: list[str],
    reason: str,
    version_only: bool = False,
) -> dict:
    affected_components = list(
        dict.fromkeys(
            component
            for provider in matrix["providers"].values()
            for component in provider["components"]
        )
    )
    affected_packs = list(
        dict.fromkeys(provider["pack"] for provider in matrix["providers"].values())
    )
    return {
        "build_all": True,
        "version_only": version_only,
        "reason": reason,
        "changed_files": changed_files,
        "affected_providers": sorted(matrix["providers"].keys()),
        "affected_components": affected_components,
        "affected_packs": affected_packs,
        "affected_manifests": sorted(
            {
                manifest
                for provider in matrix["providers"].values()
                for manifest in provider["manifests"]
            }
        ),
    }


def detect_changes(args: argparse.Namespace) -> int:
    matrix = load_matrix()
    changed_files = detect_changed_files(args.base, args.head)
    if not changed_files:
        print(json.dumps(build_all_result(matrix, changed_files, "no changed files detected")))
        return 0

    shared_paths = matrix["shared_paths"]
    owners: dict[str, set[str]] = {provider: set() for provider in matrix["providers"]}
    shared_hits: list[str] = []
    unmapped: list[str] = []
    multi_owner: list[str] = []

    for path in changed_files:
        if any(matches_path(path, candidate) for candidate in shared_paths):
            shared_hits.append(path)
            continue

        matched = [
            provider_name
            for provider_name, provider in matrix["providers"].items()
            if any(matches_path(path, candidate) for candidate in provider["paths"])
        ]
        if not matched:
            unmapped.append(path)
            continue
        if len(matched) > 1:
            multi_owner.append(path)
            continue
        owners[matched[0]].add(path)

    version_only_bump = (
        not unmapped
        and not multi_owner
        and shared_paths_are_version_only(args.base, args.head, shared_hits)
    )

    if shared_hits and not version_only_bump:
        print(json.dumps(build_all_result(
            matrix, changed_files, f"shared path changed: {shared_hits[0]}"
        )))
        return 0
    if unmapped:
        print(json.dumps(build_all_result(
            matrix, changed_files, f"unmapped path changed: {unmapped[0]}"
        )))
        return 0
    if multi_owner:
        print(json.dumps(build_all_result(
            matrix, changed_files, f"multi-provider path changed: {multi_owner[0]}"
        )))
        return 0

    if version_only_bump:
        result = build_all_result(
            matrix,
            changed_files,
            f"version bump only ({', '.join(shared_hits)})",
            version_only=True,
        )
        print(json.dumps(result))
        return 0

    affected_providers = sorted(name for name, paths in owners.items() if paths)
    affected_components = []
    affected_packs = []
    affected_manifests = []
    for provider_name in affected_providers:
        provider = matrix["providers"][provider_name]
        affected_components.extend(provider["components"])
        affected_packs.append(provider["pack"])
        affected_manifests.extend(provider["manifests"])

    result = {
        "build_all": False,
        "version_only": False,
        "reason": "provider-scoped changes only",
        "changed_files": changed_files,
        "affected_providers": affected_providers,
        "affected_components": list(dict.fromkeys(affected_components)),
        "affected_packs": list(dict.fromkeys(affected_packs)),
        "affected_manifests": list(dict.fromkeys(affected_manifests)),
    }
    print(json.dumps(result))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    resolve_parser = subparsers.add_parser("resolve-provider")
    resolve_parser.add_argument("provider")
    resolve_parser.set_defaults(func=resolve_provider)

    detect_parser = subparsers.add_parser("detect-changes")
    detect_parser.add_argument("--base", required=True)
    detect_parser.add_argument("--head", required=True)
    detect_parser.set_defaults(func=detect_changes)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
