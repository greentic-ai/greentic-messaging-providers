#!/usr/bin/env python3
"""Fast hermetic pack-spec lint.

Replaces the expensive DRY_RUN build in validate-pack-inputs with a
lightweight schema + file-reference check. Run AFTER sync_packs has
staged wasm artifacts into packs/<pack>/components/.

Catches:
  - pack.yaml missing required keys (pack_id, version, components)
  - pack_id does not match directory name
  - components[*].wasm path does not resolve to a real local file
  - components[*] missing required fields (id, wasm)

Does NOT catch:
  - subtle build-tool bugs (greentic-pack internals) -> build-packs catches
  - flow/handler validity -> flow-doctor catches
  - component descriptor errors -> component-doctor catches
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML not installed (apt install python3-yaml or pip install pyyaml)", file=sys.stderr)
    sys.exit(2)


REQUIRED_TOP_LEVEL = ("pack_id", "version", "components")
REQUIRED_COMPONENT_FIELDS = ("id", "wasm")


def load_yaml(path: Path) -> dict:
    return yaml.safe_load(path.read_text())


def resolve_wasm(wasm_ref: str, pack_dir: Path, target_dir: Path) -> Path | None:
    """Return the first existing path for a pack.yaml wasm reference.

    Checks (in order):
      1. packs/<pack>/<wasm_ref>  — relative to pack dir (post-sync state)
      2. target/components/<basename>  — fresh build-components output
    """
    primary = (pack_dir / wasm_ref).resolve()
    if primary.is_file():
        return primary
    fallback = (target_dir / Path(wasm_ref).name).resolve()
    if fallback.is_file():
        return fallback
    return None


def validate_pack(pack_name: str, packs_dir: Path, target_components: Path) -> list[str]:
    errors: list[str] = []
    pack_dir = packs_dir / pack_name
    pack_yaml = pack_dir / "pack.yaml"

    if not pack_yaml.is_file():
        return [f"pack.yaml not found at {pack_yaml}"]

    try:
        spec = load_yaml(pack_yaml)
    except yaml.YAMLError as exc:
        return [f"pack.yaml YAML parse error: {exc}"]

    if not isinstance(spec, dict):
        return [f"pack.yaml root is not a mapping: {type(spec).__name__}"]

    for key in REQUIRED_TOP_LEVEL:
        if key not in spec:
            errors.append(f"missing required top-level key: {key}")

    pack_id = spec.get("pack_id")
    if pack_id and pack_id != pack_name:
        errors.append(f"pack_id {pack_id!r} does not match directory name {pack_name!r}")

    components = spec.get("components")
    if components is None:
        return errors
    if not isinstance(components, list):
        errors.append(f"components is not a list: {type(components).__name__}")
        return errors

    for idx, comp in enumerate(components):
        if not isinstance(comp, dict):
            errors.append(f"components[{idx}] is not a mapping")
            continue
        comp_id = comp.get("id", f"<unnamed #{idx}>")
        for field in REQUIRED_COMPONENT_FIELDS:
            if field not in comp:
                errors.append(f"components[{idx}] ({comp_id}): missing field {field!r}")

        wasm_ref = comp.get("wasm")
        if not wasm_ref:
            continue
        resolved = resolve_wasm(wasm_ref, pack_dir, target_components)
        if resolved is None:
            errors.append(
                f"components[{idx}] ({comp_id}): wasm reference {wasm_ref!r} does "
                f"not resolve (checked pack dir and {target_components})"
            )

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--packs", nargs="+", required=True, help="pack names to lint")
    parser.add_argument("--packs-dir", default="packs", help="root dir holding packs/")
    parser.add_argument(
        "--components-dir",
        default="target/components",
        help="fallback dir for wasm artifacts if pack-relative path missing",
    )
    parser.add_argument("--report", help="optional JSON report output path")
    args = parser.parse_args()

    packs_dir = Path(args.packs_dir).resolve()
    target_components = Path(args.components_dir).resolve()

    all_errors: dict[str, list[str]] = {}
    for pack in args.packs:
        errors = validate_pack(pack, packs_dir, target_components)
        if errors:
            all_errors[pack] = errors
            print(f"FAIL {pack}:", file=sys.stderr)
            for err in errors:
                print(f"  - {err}", file=sys.stderr)
        else:
            print(f"OK   {pack}")

    if args.report:
        report_path = Path(args.report)
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "status": "error" if all_errors else "ok",
                    "packs_checked": args.packs,
                    "errors": all_errors,
                },
                indent=2,
            )
            + "\n"
        )

    return 1 if all_errors else 0


if __name__ == "__main__":
    sys.exit(main())
