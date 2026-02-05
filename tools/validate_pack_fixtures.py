#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Dict, List, Set

import yaml


REQUIRED_ALWAYS = {
    "requirements.expected.json",
    "setup.input.json",
    "setup.expected.plan.json",
    "egress.request.json",
    "egress.expected.summary.json",
}

REQUIRED_INGRESS = {
    "ingress.request.json",
    "ingress.expected.message.json",
}

REQUIRED_SUBSCRIPTIONS = {
    "subscriptions.desired.json",
    "subscriptions.expected.ops.json",
}


def load_json(path: Path) -> Dict:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        raise SystemExit(f"missing fixture: {path}")
    except json.JSONDecodeError as exc:
        raise SystemExit(f"invalid json in {path}: {exc}")


def infer_pack_capabilities(pack_dir: Path) -> Dict[str, bool]:
    data = yaml.safe_load((pack_dir / "pack.yaml").read_text())
    flows = data.get("flows") or []
    ingress = any((flow.get("id") == "verify_webhooks") for flow in flows if isinstance(flow, dict))
    providers = (
        (data.get("extensions") or {})
        .get("greentic.provider-extension.v1", {})
        .get("inline", {})
        .get("providers", [])
    )
    egress = False
    for provider in providers:
        ops = provider.get("ops") or []
        if any(op in ("send", "reply") for op in ops):
            egress = True
    subscriptions = (data.get("extensions") or {}).get("messaging.subscriptions.v1") is not None
    return {"ingress": ingress, "egress": egress, "subscriptions": subscriptions}


def validate_structure(pack_dir: Path, fixtures_dir: Path, capabilities: Dict[str, bool]) -> List[str]:
    errors: List[str] = []

    required = set(REQUIRED_ALWAYS)
    if capabilities["ingress"]:
        required |= REQUIRED_INGRESS
    if capabilities["subscriptions"]:
        required |= REQUIRED_SUBSCRIPTIONS

    for name in required:
        path = fixtures_dir / name
        if not path.exists():
            errors.append(f"{pack_dir.name}: missing {name}")
            continue
        payload = load_json(path)
        if name == "requirements.expected.json":
            for key in ["provider", "config_required", "secret_required"]:
                if key not in payload:
                    errors.append(f"{pack_dir.name}: {name} missing {key}")
        if name == "setup.input.json":
            for key in ["mode", "config", "secrets"]:
                if key not in payload:
                    errors.append(f"{pack_dir.name}: {name} missing {key}")
        if name == "setup.expected.plan.json":
            for key in ["config_patch", "secrets_patch", "webhook_ops", "subscription_ops", "oauth_ops"]:
                if key not in payload:
                    errors.append(f"{pack_dir.name}: {name} missing {key}")
        if name == "ingress.request.json":
            for key in ["event_id", "payload"]:
                if key not in payload:
                    errors.append(f"{pack_dir.name}: {name} missing {key}")
        if name == "ingress.expected.message.json":
            if "message" not in payload:
                errors.append(f"{pack_dir.name}: {name} missing message")
        if name == "egress.request.json":
            if "message" not in payload:
                errors.append(f"{pack_dir.name}: {name} missing message")
        if name == "egress.expected.summary.json":
            for key in ["status", "message_id"]:
                if key not in payload:
                    errors.append(f"{pack_dir.name}: {name} missing {key}")
        if name == "subscriptions.desired.json":
            if "subscriptions" not in payload:
                errors.append(f"{pack_dir.name}: {name} missing subscriptions")
        if name == "subscriptions.expected.ops.json":
            if "ops" not in payload:
                errors.append(f"{pack_dir.name}: {name} missing ops")

    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    packs_dir = root / "packs"
    failures: List[str] = []
    report: Dict[str, Dict[str, List[str]]] = {}

    for pack_dir in sorted(p for p in packs_dir.iterdir() if p.is_dir() and p.name.startswith("messaging-")):
        fixtures_dir = pack_dir / "fixtures"
        if not fixtures_dir.exists():
            report[pack_dir.name] = {"errors": []}
            continue
        caps = infer_pack_capabilities(pack_dir)
        errors = validate_structure(pack_dir, fixtures_dir, caps)
        if errors:
            failures.extend(errors)
            report[pack_dir.name] = {"errors": errors}
        else:
            report[pack_dir.name] = {"errors": []}

    report_path = root / "dist" / "fixtures_report.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n")

    if failures:
        sys.stderr.write("Fixture validation failed:\n")
        for error in failures:
            sys.stderr.write(f"- {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
