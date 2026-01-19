#!/usr/bin/env python3
"""
Generate pack.manifest.json files with aggregated secret requirements.

Reads the pack manifest for a given pack directory, walks the referenced
components to collect their secret_requirements, deduplicates them by
name/scope (keeping the first non-empty description/example), and writes the
updated manifest back. Optionally overrides the manifest version.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple

import yaml

PROVIDER_EXTENSION_ID = "greentic.provider-extension.v1"

def load_json(path: Path) -> dict:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError as exc:
        sys.stderr.write(f"missing manifest at {path}: {exc}\n")
        raise SystemExit(1)
    except json.JSONDecodeError as exc:
        sys.stderr.write(f"invalid json in {path}: {exc}\n")
        raise SystemExit(1)


def dedupe_requirements(requirements: Iterable[dict]) -> List[dict]:
    merged: Dict[Tuple[str, str], dict] = {}
    for req in requirements:
        name = req.get("name")
        if not name:
            continue
        scope = req.get("scope") or "tenant"
        description = req.get("description") or ""
        example = req.get("example") or ""
        key = (name, scope)
        existing = merged.get(key)
        if existing:
            description = existing.get("description") or description
            example = existing.get("example") or example
        merged[key] = {"name": name, "scope": scope}
        if description:
            merged[key]["description"] = description
        if example:
            merged[key]["example"] = example
    return list(merged.values())


def aggregate_requirements(pack_dir: Path, components_dir: Path) -> List[dict]:
    manifest = load_json(pack_dir / "pack.manifest.json")
    components = (
        manifest.get("component_sources")
        or manifest.get("components")
        or []
    )
    reqs: List[dict] = []
    for component in components:
        if isinstance(component, dict):
            comp_id = component.get("id")
            # Skip remote/OCI components when we don't have a local manifest copy.
            if not comp_id or component.get("oci"):
                continue
        else:
            comp_id = component
        comp_manifest = components_dir / comp_id / "component.manifest.json"
        if comp_id == "handlebars":
            alt_manifest = components_dir / "templates" / "component.manifest.json"
            if alt_manifest.exists():
                comp_manifest = alt_manifest
        if not comp_manifest.exists():
            if "__" in comp_id:
                continue
            sys.stderr.write(
                f"warning: component manifest not found for {comp_id} at {comp_manifest}\n"
            )
            continue
        data = load_json(comp_manifest)
        component_reqs = data.get("secret_requirements") or []
        reqs.extend(component_reqs)
    # allow manual/static requirements already in the pack manifest
    reqs.extend(manifest.get("secret_requirements") or [])
    return dedupe_requirements(reqs)


def normalize_provider_extension(manifest: dict) -> None:
    """
    Ensure the provider extension uses the canonical greentic-types identifier.
    """
    extensions = manifest.get("extensions") or {}
    if not isinstance(extensions, dict):
        return

    def coerce_kind(entry: dict) -> None:
        if isinstance(entry, dict):
            entry["kind"] = PROVIDER_EXTENSION_ID

    if PROVIDER_EXTENSION_ID in extensions:
        coerce_kind(extensions[PROVIDER_EXTENSION_ID])
        manifest["extensions"] = extensions
        return

    legacy_keys = [k for k in extensions.keys() if isinstance(k, str) and k.startswith("greentic.ext.provider")]
    if legacy_keys:
        legacy_key = legacy_keys[0]
        extensions[PROVIDER_EXTENSION_ID] = extensions.pop(legacy_key)
        coerce_kind(extensions[PROVIDER_EXTENSION_ID])
        manifest["extensions"] = extensions

def write_manifest(manifest_path: Path, manifest: dict) -> None:
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")


def include_capabilities_cache(
    manifest: dict, pack_dir: Path, components_dir: Path
) -> None:
    cache_entries = []
    components = manifest.get("components") or []
    cache_out_dir = pack_dir / "components"
    cache_out_dir.mkdir(parents=True, exist_ok=True)
    for component in components:
        comp_id = component.get("id") if isinstance(component, dict) else component
        src = components_dir / comp_id / "capabilities_v1.json"
        if not src.exists():
            continue
        dest = cache_out_dir / f"{comp_id}-capabilities_v1.json"
        shutil.copyfile(src, dest)
        cache_entries.append(
            {"component": comp_id, "version": "v1", "path": f"components/{dest.name}"}
        )
    if cache_entries:
        manifest["capabilities_cache"] = cache_entries


def main() -> int:
    parser = argparse.ArgumentParser(description="Aggregate pack secret requirements.")
    parser.add_argument(
        "--pack-dir",
        required=True,
        type=Path,
        help="Path to the pack directory containing pack.manifest.json",
    )
    parser.add_argument(
        "--components-dir",
        type=Path,
        help="Path to the components directory (defaults to ../../components from the pack dir)",
    )
    parser.add_argument(
        "--version",
        help="Optional version override to stamp into pack.manifest.json",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="Optional output path (defaults to pack.manifest.json in the pack directory)",
    )
    parser.add_argument(
        "--secrets-out",
        type=Path,
        help="Optional path to write aggregated secret requirements JSON array for pack builders",
    )
    parser.add_argument(
        "--include-capabilities-cache",
        action="store_true",
        help="If set, copy capabilities_v1.json from component directories into the pack and reference them in pack.manifest.json",
    )
    args = parser.parse_args()

    pack_dir = args.pack_dir
    components_dir = args.components_dir or pack_dir.parent.parent / "components"
    manifest_path = args.output or pack_dir / "pack.manifest.json"

    if not pack_dir.exists():
        sys.stderr.write(f"pack directory not found: {pack_dir}\n")
        return 1

    # Load pack.yaml to align top-level kind/version/components.
    pack_yaml_path = pack_dir / "pack.yaml"
    pack_yaml = yaml.safe_load(pack_yaml_path.read_text()) if pack_yaml_path.exists() else {}

    secret_requirements = aggregate_requirements(pack_dir, components_dir)
    manifest = load_json(pack_dir / "pack.manifest.json")

    # Bring manifest basics in sync with pack.yaml.
    if pack_yaml:
        manifest["name"] = pack_yaml.get("pack_id", manifest.get("name", ""))
        manifest["publisher"] = pack_yaml.get(
            "publisher", manifest.get("publisher", "Greentic")
        )
        manifest["kind"] = pack_yaml.get("kind", manifest.get("kind", "application"))
        manifest["version"] = pack_yaml.get("version", manifest.get("version", "0.0.0"))
        if pack_yaml.get("extensions"):
            manifest["extensions"] = pack_yaml.get("extensions", {})

        component_ids = []
        component_sources = []
        for comp in pack_yaml.get("components", []) or []:
            if not isinstance(comp, dict):
                continue
            comp_id = comp.get("id")
            if not comp_id:
                continue
            comp_copy = dict(comp)
            comp_copy.setdefault("wasm", f"components/{comp_id}.wasm")
            component_sources.append(comp_copy)
            # Only include components without an OCI reference in the manifest's
            # top-level components list (legacy consumers expect local manifests).
            if not comp_copy.get("oci"):
                comp_manifest = components_dir / comp_id / "component.manifest.json"
                if comp_manifest.exists():
                    component_ids.append(comp_id)
        if component_ids:
            manifest["components"] = component_ids
            manifest["component_sources"] = component_sources

        schemas = pack_yaml.get("schemas") or []
        if isinstance(schemas, list):
            for schema in schemas:
                if not isinstance(schema, dict):
                    continue
                if schema.get("kind") == "config" and schema.get("path"):
                    manifest["config_schema"] = {
                        "provider_config": {
                            "format": "json",
                            "path": schema.get("path"),
                        }
                    }
        flows = pack_yaml.get("flows")
        if isinstance(flows, list) and flows:
            manifest["flows"] = flows

    normalize_provider_extension(manifest)
    manifest["secret_requirements"] = secret_requirements

    if args.include_capabilities_cache:
        include_capabilities_cache(manifest, pack_dir, components_dir)
    if args.version:
        manifest["version"] = args.version

    write_manifest(manifest_path, manifest)
    if args.secrets_out:
        # PackC expects scope as a structured enum, not a string.
        def scope_struct(scope: str) -> dict:
            normalized = (scope or "tenant").lower()
            env_val = "<env>"
            tenant_val = "<tenant>"
            team_val = "<team>"
            if normalized == "team":
                return {"env": env_val, "tenant": tenant_val, "team": team_val}
            if normalized in ("env", "environment"):
                return {"env": env_val, "tenant": tenant_val}
            return {"env": env_val, "tenant": tenant_val}

        bridged = []
        for req in secret_requirements:
            scoped = dict(req)
            scoped["key"] = scoped.pop("name", scoped.get("key", ""))
            scoped["scope"] = scope_struct(req.get("scope", "tenant"))
            bridged.append(scoped)
        args.secrets_out.write_text(json.dumps(bridged, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
