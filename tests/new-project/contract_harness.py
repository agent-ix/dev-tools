"""Fixture-only host harness for the new-project assurance handoff contract."""

from __future__ import annotations

import copy
import hashlib
import json
import re
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

PLUGIN_IDENTITY = "engineering-assurance"
SKILL_NAME = "assurance-onboarding"
HANDOFF_CONTRACT = "agent-ix.assurance-onboarding-handoff/v1"
RESULT_CONTRACT = "agent-ix.assurance-onboarding-handoff-result/v1"
SEMVER = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")


class ProviderFailure(RuntimeError):
    """Represent a host invocation failure with opaque provider diagnostics."""

    def __init__(self, diagnostic: Any) -> None:
        super().__init__("fixture provider failed")
        self.diagnostic = diagnostic


@dataclass(frozen=True)
class ResolvedSkill:
    """Provider identity resolved entirely through installed-plugin metadata."""

    identity: str
    version: str
    skill_name: str
    skill_file: Path
    invoke: Callable[[dict[str, Any]], Any]


class FixtureHost:
    """Minimal plugin discovery and invocation seam used only by contract tests."""

    def __init__(
        self,
        plugin_root: Path | None,
        installation_help: str,
        adapters: dict[str, Any] | None = None,
    ) -> None:
        self.plugin_root = plugin_root
        self.installation_help = installation_help
        self.adapters = adapters or {}
        self.discovery_count = 0
        self.invocation_count = 0
        self.observed_provider: dict[str, str] | None = None

    def discover(self) -> tuple[ResolvedSkill | None, str | None, str | None]:
        """Resolve the provider or return a stable reason code and diagnostic."""
        self.discovery_count += 1
        if self.plugin_root is None:
            return None, "plugin_unavailable", "plugin is not installed"

        root = self.plugin_root.resolve()
        manifest_path = root / ".codex-plugin" / "plugin.json"
        if not manifest_path.is_file():
            return None, "plugin_unavailable", "plugin manifest is absent"
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            return None, "entrypoint_incompatible", str(error)
        if not isinstance(manifest, dict):
            return None, "entrypoint_incompatible", "plugin manifest is not an object"
        observed = {
            key: value
            for key in ("name", "version")
            if isinstance((value := manifest.get(key)), str)
        }
        self.observed_provider = observed or None
        if manifest.get("name") != PLUGIN_IDENTITY:
            return None, "entrypoint_incompatible", "plugin identity is incompatible"

        version = manifest.get("version")
        if not isinstance(version, str) or SEMVER.fullmatch(version) is None:
            return None, "entrypoint_incompatible", "plugin version is not immutable"
        skill_source = manifest.get("skills")
        if not isinstance(skill_source, str) or not skill_source.strip():
            return None, "entrypoint_incompatible", "skill source is missing"

        declared_source = Path(skill_source)
        if declared_source.is_absolute():
            return None, "entrypoint_incompatible", "skill source must be relative"
        source = (root / declared_source).resolve()
        if not source.is_relative_to(root):
            return None, "entrypoint_incompatible", "skill source escapes plugin root"
        if not source.is_dir():
            return None, "capability_missing", "declared skill source is absent"

        matches = []
        for skill_file in source.glob("*/SKILL.md"):
            try:
                frontmatter = _frontmatter(skill_file)
            except (OSError, ValueError) as error:
                return None, "entrypoint_incompatible", str(error)
            if frontmatter.get("name") == SKILL_NAME:
                matches.append(skill_file.resolve())
        if len(matches) != 1:
            return (
                None,
                "capability_missing",
                "assurance-onboarding skill is not unique",
            )

        adapter = self.adapters.get(SKILL_NAME)
        if not callable(adapter):
            return (
                None,
                "entrypoint_incompatible",
                "host skill entry point is not callable",
            )

        def invoke(payload: dict[str, Any]) -> Any:
            self.invocation_count += 1
            return adapter(payload)

        return (
            ResolvedSkill(PLUGIN_IDENTITY, version, SKILL_NAME, matches[0], invoke),
            None,
            None,
        )


def _frontmatter(path: Path) -> dict[str, str]:
    text = path.read_text(encoding="utf-8")
    if not text.startswith("---\n"):
        return {}
    parts = text.split("---", 2)
    if len(parts) != 3:
        raise ValueError("skill frontmatter closing delimiter is missing")
    _, block, _ = parts
    fields: dict[str, str] = {}
    for line in block.splitlines():
        key, separator, value = line.partition(":")
        if separator:
            fields[key.strip()] = value.strip()
    return fields


def run_handoff(payload: dict[str, Any], host: FixtureHost | None) -> dict[str, Any]:
    """Exercise the generic consumer contract without provider-owned behavior."""
    _validate_payload(payload)
    recorded_payload = copy.deepcopy(payload)
    verification = recorded_payload["verification"]
    decision = recorded_payload["decision"]
    if verification["outcome"] != "passed":
        return _result(
            recorded_payload,
            "deferred",
            "scaffold_verification_failed",
            "scaffold verification did not pass",
        )
    if decision["outcome"] != "invoke":
        return _result(
            recorded_payload,
            decision["outcome"],
            f"explicit_{decision['outcome']}",
            decision["reason"],
        )
    if host is None:
        return _result(
            recorded_payload,
            "deferred",
            "plugin_unavailable",
            "the host exposes no plugin system",
        )

    resolved, reason_code, diagnostic = host.discover()
    if resolved is None:
        result = _result(recorded_payload, "deferred", reason_code, diagnostic)
        if host.observed_provider is not None:
            result["observed_provider"] = host.observed_provider
        if reason_code == "plugin_unavailable":
            result["installation_help"] = host.installation_help
        return result

    provider = {
        "identity": resolved.identity,
        "version": resolved.version,
        "skill": resolved.skill_name,
    }
    try:
        provider_result = resolved.invoke(copy.deepcopy(recorded_payload))
    except ProviderFailure as error:
        result = _result(
            recorded_payload,
            "failed",
            "provider_failed",
            "installed provider invocation failed",
        )
        result["provider"] = provider
        result["provider_result"] = error.diagnostic
        return result
    except Exception as error:  # noqa: BLE001
        # This injected external-provider boundary must normalize arbitrary
        # provider exceptions. BaseException process-control signals still pass.
        result = _result(
            recorded_payload,
            "failed",
            "provider_failed",
            "installed provider invocation failed",
        )
        result["provider"] = provider
        result["provider_result"] = str(error)
        return result

    result = _result(recorded_payload, "completed")
    result["provider"] = provider
    result["provider_result"] = provider_result
    return result


def _validate_payload(payload: dict[str, Any]) -> None:
    required = {
        "contract",
        "repository",
        "template_type",
        "verification",
        "requested_context",
        "decision",
    }
    if set(payload) != required or payload.get("contract") != HANDOFF_CONTRACT:
        raise ValueError("handoff payload shape is incompatible")
    repository = payload["repository"]
    if set(repository) != {"organization", "name", "path", "remote_url"}:
        raise ValueError("repository identity shape is incompatible")
    if not Path(repository["path"]).is_absolute():
        raise ValueError("repository path must be absolute")
    verification = payload["verification"]
    if set(verification) != {"outcome", "checks"}:
        raise ValueError("verification result shape is incompatible")
    if verification["outcome"] not in {"passed", "failed", "incomplete"}:
        raise ValueError("verification outcome is incompatible")
    for check in verification["checks"]:
        if set(check) != {"command", "exit_code", "summary"}:
            raise ValueError("verification check shape is incompatible")
    decision = payload["decision"]
    if decision.get("outcome") not in {
        "invoke",
        "deferred",
        "skipped",
        "not_applicable",
    }:
        raise ValueError("decision outcome is incompatible")
    if decision["outcome"] != "invoke" and not decision.get("reason", "").strip():
        raise ValueError("non-invocation decisions require a reason")


def _result(
    payload: dict[str, Any],
    outcome: str,
    reason_code: str | None = None,
    reason: str | None = None,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "contract": RESULT_CONTRACT,
        "input": payload,
        "outcome": outcome,
    }
    if reason_code is not None:
        result["reason_code"] = reason_code
    if reason is not None:
        result["reason"] = reason
    return result


def tree_digest(root: Path) -> str:
    """Return a stable content digest for every regular file in a fixture tree."""
    digest = hashlib.sha256()
    for path in sorted(
        candidate for candidate in root.rglob("*") if candidate.is_file()
    ):
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


# Names of provider-owned skills and trees. A directory carrying one of these
# is a copied provider asset, which FR-002-AC-5 forbids outright.
PROVIDER_PATH_PARTS = frozenset(
    {
        "architecture-evaluation",
        "assurance-intake",
        "assurance-onboarding",
        "change-assurance",
        "engineering-assurance",
        "engineering_assurance",
        "measurement-promotion",
    }
)

# Markers of provider *implementation*: executing an assurance workflow, or
# carrying a copy of the provider's own skill body.
PROVIDER_IMPLEMENTATION_MARKERS = frozenset(
    {
        "# Assurance onboarding",
        "## Inventory before proposing",
        "engineering_assurance.workflow",
        "name: assurance-onboarding",
        "run_binding",
        "decision_ready",
    }
)

# CON-2 forbids *writing* an assurance artifact. Naming one is not writing it:
# the generic layer must describe what it hands off to, and `code-review` is
# required to look for an AssuranceProfile by design. Match the structured
# declaration that makes a file an artifact, not the vocabulary.
PROVIDER_ARTIFACT_DECLARATION = re.compile(
    r"^\s*(?:kind|type|apiVersion)\s*:\s*[\"']?"
    r"(AssuranceProfile|MeasurementPlan)\b",
    re.MULTILINE,
)


def provider_ownership_violations(root: Path) -> list[str]:
    """Find provider-owned assets or implementations outside the test fixtures.

    Enforces FR-002-AC-5 (no provider asset tree or skill copy) and
    FR-002-CON-1 (generic production paths stay ownership-clean) by what they
    say, rather than by enumerating every file the repository is allowed to
    contain. An allowlist of permitted production files fails every time an
    unrelated file is added -- which is how a rights preflight and a content
    policy turned this check red.
    """
    violations: list[str] = []
    for path in sorted(c for c in root.rglob("*") if c.is_file()):
        relative = path.relative_to(root)
        parts = relative.parts
        # Fixtures deliberately contain a synthetic provider plugin.
        if "fixtures" in parts:
            continue

        lowered = {part.lower() for part in parts}
        if PROVIDER_PATH_PARTS.intersection(lowered):
            violations.append(f"provider-owned path: {relative.as_posix()}")
            continue

        # The audit itself names the markers it searches for.
        if "tests" in parts:
            continue

        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        for marker in sorted(PROVIDER_IMPLEMENTATION_MARKERS):
            if marker in text:
                violations.append(
                    f"provider-owned implementation {marker!r}: {relative.as_posix()}"
                )
        declared = PROVIDER_ARTIFACT_DECLARATION.search(text)
        if declared:
            violations.append(
                f"written assurance artifact {declared.group(1)!r}: "
                f"{relative.as_posix()}"
            )
    return violations
