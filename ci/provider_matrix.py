#!/usr/bin/env python3
import argparse
import json
import subprocess
import sys
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT_DIR / "ci" / "provider-matrix.json"


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


def build_all_result(matrix: dict, changed_files: list[str], reason: str) -> dict:
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

    for path in changed_files:
        if any(matches_path(path, candidate) for candidate in shared_paths):
            print(json.dumps(build_all_result(matrix, changed_files, f"shared path changed: {path}")))
            return 0

        matched = [
            provider_name
            for provider_name, provider in matrix["providers"].items()
            if any(matches_path(path, candidate) for candidate in provider["paths"])
        ]
        if not matched:
            print(json.dumps(build_all_result(matrix, changed_files, f"unmapped path changed: {path}")))
            return 0
        if len(matched) > 1:
            print(json.dumps(build_all_result(matrix, changed_files, f"multi-provider path changed: {path}")))
            return 0
        owners[matched[0]].add(path)

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
