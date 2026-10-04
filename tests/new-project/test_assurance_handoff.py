"""Executable traceability for TM-001's optional assurance handoff."""

from __future__ import annotations

import copy
import json
import shutil
import tempfile
from pathlib import Path
from typing import Any

from contract_harness import (
    PROVIDER_ARTIFACT_DECLARATION,
    PROVIDER_IMPLEMENTATION_MARKERS,
    FixtureHost,
    ProviderFailure,
    provider_ownership_violations,
    run_handoff,
    tree_digest,
)

HERE = Path(__file__).resolve().parent
REPOSITORY_ROOT = HERE.parent.parent
SKILL_ROOT = REPOSITORY_ROOT / "skills" / "new-project"
FIXTURE = HERE / "fixtures" / "engineering-assurance"
INSTALL_HELP = "Install the fixture from its module-owned catalog entry."


def payload(
    root: Path, outcome: str = "passed", decision: str = "invoke"
) -> dict[str, Any]:
    return {
        "contract": "agent-ix.assurance-onboarding-handoff/v1",
        "repository": {
            "organization": "agent-ix",
            "name": "fixture-project",
            "path": str(root.resolve()),
            "remote_url": "git@example.invalid:agent-ix/fixture-project.git",
        },
        "template_type": "python-lib-cookiecutter",
        "verification": {
            "outcome": outcome,
            "checks": [
                {"command": "make test", "exit_code": 0, "summary": "42 passed"},
                {"command": "make build", "exit_code": 0, "summary": "built"},
            ],
        },
        "requested_context": {"decision_boundary": "release readiness"},
        "decision": {
            "outcome": decision,
            "reason": "operator selected this outcome" if decision != "invoke" else "",
        },
    }


class TestHandoffContract:
    """Verify the generic handoff against a minimal injected host boundary."""

    def setup_method(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.scaffold = self.root / "scaffold"
        self.scaffold.mkdir()
        (self.scaffold / "README.md").write_text(
            "verified scaffold\n", encoding="utf-8"
        )
        self.plugin = self.root / "plugin"
        shutil.copytree(FIXTURE, self.plugin)
        self.received: list[dict[str, Any]] = []

    def teardown_method(self) -> None:
        self.temporary.cleanup()

    def host(self, adapter: Any | None = None) -> FixtureHost:
        if adapter is None:
            adapter = lambda value: self.received.append(value) or '{"status":"ok"}'
        return FixtureHost(
            self.plugin,
            INSTALL_HELP,
            {"assurance-onboarding": adapter},
        )

    def test_tc_001_compatible_fixture_receives_exact_payload_once(self) -> None:
        """Description:
            Verify one exact compatible handoff. TC-001, FR-001, IT-001.
        Assumptions:
            - Scaffold verification passed and the fixture entry point is callable.
        Criteria:
            - FR-001-AC-1: The adapter receives the unchanged payload exactly once.
            - IT-001-SC-01: The generic outcome is completed.
        """
        handoff = payload(self.scaffold)
        original = copy.deepcopy(handoff)
        received_snapshots: list[dict[str, Any]] = []
        received_objects: list[dict[str, Any]] = []

        def mutating_adapter(value: dict[str, Any]) -> str:
            received_snapshots.append(copy.deepcopy(value))
            received_objects.append(value)
            value["requested_context"]["provider_mutation"] = True
            return '{"status":"ok"}'

        host = self.host(mutating_adapter)

        result = run_handoff(handoff, host)

        assert received_snapshots == [original]
        assert received_objects[0] is not handoff
        assert received_objects[0] is not result["input"]
        assert handoff == original
        assert result["input"] == original
        assert host.invocation_count == 1
        assert result["outcome"] == "completed"

    def test_tc_002_opaque_provider_results_round_trip_exactly(self) -> None:
        """Description:
            Verify opaque provider results are not translated. TC-002, FR-001, IT-001.
        Assumptions:
            - The fixture returns opaque JSON or raises typed and ordinary errors.
        Criteria:
            - FR-001-AC-2: Completed and failed provider data is byte-identical.
            - IT-001-SC-02: Provider failure retains its distinct generic outcome.
        """
        completed = '{"nested":[1,2],"spacing": "retained"}'
        failed = '{"provider":"diagnostic","raw": true}'

        success = run_handoff(payload(self.scaffold), self.host(lambda _: completed))
        failure = run_handoff(
            payload(self.scaffold),
            self.host(lambda _: (_ for _ in ()).throw(ProviderFailure(failed))),
        )
        ordinary_failure = run_handoff(
            payload(self.scaffold),
            self.host(lambda _: (_ for _ in ()).throw(RuntimeError("boom"))),
        )
        value_failure = run_handoff(
            payload(self.scaffold),
            self.host(lambda _: (_ for _ in ()).throw(ValueError("bad value"))),
        )
        os_failure = run_handoff(
            payload(self.scaffold),
            self.host(lambda _: (_ for _ in ()).throw(OSError("host io"))),
        )

        assert success["provider_result"] == completed
        assert failure["provider_result"] == failed
        assert failure["outcome"] == "failed"
        assert failure["reason_code"] == "provider_failed"
        assert ordinary_failure["outcome"] == "failed"
        assert ordinary_failure["reason_code"] == "provider_failed"
        assert ordinary_failure["provider_result"] == "boom"
        assert value_failure["reason_code"] == "provider_failed"
        assert value_failure["provider_result"] == "bad value"
        assert os_failure["reason_code"] == "provider_failed"
        assert os_failure["provider_result"] == "host io"

    def test_tc_003_failed_verification_prevents_discovery_and_invocation(self) -> None:
        """Description:
            Refuse onboarding after failed scaffold verification. TC-003, FR-001.
        Assumptions:
            - A discoverable fixture host exists but verification failed or is incomplete.
        Criteria:
            - FR-001-AC-3: The result is scaffold_verification_failed.
            - IT-001-SC-04: Discovery and invocation counts remain zero.
        """
        for outcome in ("failed", "incomplete"):
            host = self.host()
            result = run_handoff(payload(self.scaffold, outcome=outcome), host)
            assert result["reason_code"] == "scaffold_verification_failed", outcome
            assert host.discovery_count == 0, outcome
            assert host.invocation_count == 0, outcome

        skill = (SKILL_ROOT / "SKILL.md").read_text(encoding="utf-8")
        assert "failed or incomplete verification" in skill
        assert "scaffold_verification_failed" in skill

    def test_tc_004_explicit_non_invocation_decisions_preserve_reasons(self) -> None:
        """Description:
            Preserve every explicit non-invocation decision. TC-004, FR-001.
        Assumptions:
            - Each decision includes a non-empty operator reason.
        Criteria:
            - FR-001-AC-4: Outcome and reason round-trip unchanged.
            - IT-001-SC-03: No provider is invoked.
        """
        for decision in ("deferred", "skipped", "not_applicable"):
            host = self.host()
            handoff = payload(self.scaffold, decision=decision)
            result = run_handoff(handoff, host)
            assert result["outcome"] == decision
            assert result["reason"] == handoff["decision"]["reason"]
            assert host.invocation_count == 0

    def test_tc_005_payload_has_no_provider_policy_fields(self) -> None:
        """Description:
            Keep provider policy fields out of the generic payload. TC-005, FR-001.
        Assumptions:
            - The canonical fixture payload contains all required generic fields.
        Criteria:
            - FR-001-AC-5: No provider artifact, engine, or policy field is required.
        """
        handoff = payload(self.scaffold)

        serialized = json.dumps(handoff).lower()

        for forbidden in (
            "assuranceprofile",
            "measurementplan",
            "quire",
            "quoin",
            "ix-flow",
        ):
            assert forbidden not in serialized

    def test_tc_006_unavailable_and_failed_paths_preserve_scaffold(self) -> None:
        """Description:
            Preserve the scaffold on unavailable and failed paths. TC-006, FR-001.
        Assumptions:
            - A stable digest represents the already verified scaffold.
        Criteria:
            - FR-001-AC-6: Every case leaves the scaffold byte-identical.
            - IT-001-SC-10: Failure does not roll back scaffold completion.
        """
        before = tree_digest(self.scaffold)
        missing = self.root / "digest-missing-capability"
        shutil.copytree(self.plugin, missing)
        shutil.rmtree(missing / "skills" / "assurance-onboarding")
        incompatible = self.root / "digest-incompatible"
        shutil.copytree(self.plugin, incompatible)
        manifest_path = incompatible / ".codex-plugin" / "plugin.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["skills"] = "../outside"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        cases = (
            (payload(self.scaffold), FixtureHost(None, INSTALL_HELP)),
            (
                payload(self.scaffold),
                self.host(lambda _: (_ for _ in ()).throw(ProviderFailure("x"))),
            ),
            (
                payload(self.scaffold),
                FixtureHost(missing, INSTALL_HELP, {}),
            ),
            (
                payload(self.scaffold),
                FixtureHost(incompatible, INSTALL_HELP, {}),
            ),
            (payload(self.scaffold, outcome="failed"), self.host()),
            (payload(self.scaffold, decision="deferred"), self.host()),
            (payload(self.scaffold, decision="skipped"), self.host()),
            (payload(self.scaffold, decision="not_applicable"), self.host()),
        )

        for handoff, host in cases:
            run_handoff(handoff, host)
            assert tree_digest(self.scaffold) == before

    def test_tc_007_generic_integration_has_no_engine_or_artifact_writer(self) -> None:
        """Description:
            Audit the generic integration boundary. TC-007, FR-001.
        Assumptions:
            - Production behavior is the skill and its referenced protocol.
        Criteria:
            - FR-001-AC-7: The handoff exists without a production engine script.
            - FR-001-CON-1: The skill contains no direct engine invocation.
            - FR-001-CON-2: The generic surface contains no artifact writer.
        """
        # CON-1 and CON-2 forbid invoking an assurance engine and writing an
        # assurance artifact. They do not forbid the skill owning a script:
        # the rights preflight is one, and it has nothing to do with assurance.
        # Assert the boundary these criteria name, not the file inventory.
        production = [
            path for path in SKILL_ROOT.rglob("*") if path.is_file() and "tests" not in path.parts
        ]
        assert production, "the skill must have production files to audit"
        engine_calls = [
            f"{path.relative_to(SKILL_ROOT).as_posix()}: {marker!r}"
            for path in production
            for marker in sorted(PROVIDER_IMPLEMENTATION_MARKERS)
            if marker in path.read_text(encoding="utf-8", errors="replace")
        ]
        assert engine_calls == []
        artifact_writers = [
            path.relative_to(SKILL_ROOT).as_posix()
            for path in production
            if PROVIDER_ARTIFACT_DECLARATION.search(
                path.read_text(encoding="utf-8", errors="replace")
            )
        ]
        assert artifact_writers == []
        skill = (SKILL_ROOT / "SKILL.md").read_text(encoding="utf-8")
        protocol = SKILL_ROOT / "references" / "assurance-onboarding-handoff.md"
        assert "Post-verification assurance handoff" in skill
        assert "references/assurance-onboarding-handoff.md" in skill
        assert protocol.is_file()
        protocol_text = protocol.read_text(encoding="utf-8")
        for required in (
            "agent-ix.assurance-onboarding-handoff/v1",
            "agent-ix.assurance-onboarding-handoff-result/v1",
            "plugin_unavailable",
            "capability_missing",
            "entrypoint_incompatible",
            "provider_failed",
        ):
            assert required in protocol_text
        assert "ix-flow run" not in skill
        assert "`quire " not in skill.lower()
        assert "`quoin " not in skill.lower()
        assert "ix-flow run" not in protocol_text
        assert "`quire " not in protocol_text.lower()
        assert "`quoin " not in protocol_text.lower()

    def test_tc_008_exact_metadata_resolves_one_callable_entry_point(self) -> None:
        """Description:
            Resolve the exact installed provider entry point. TC-008, FR-002.
        Assumptions:
            - The fixture matches the provider's published manifest shape.
        Criteria:
            - FR-002-AC-1: Identity, version, skill, and root-confined path match.
        """
        host = self.host()

        resolved, reason, diagnostic = host.discover()

        assert resolved is not None
        assert resolved.identity == "engineering-assurance"
        assert resolved.version == "0.2.0"
        assert resolved.skill_name == "assurance-onboarding"
        assert resolved.skill_file.is_relative_to(self.plugin.resolve())
        assert reason is None
        assert diagnostic is None

    def test_tc_009_missing_provider_defers_with_catalog_help(self) -> None:
        """Description:
            Defer a missing provider with catalog-owned help. TC-009, FR-002.
        Assumptions:
            - The host catalog supplies installation guidance separately.
        Criteria:
            - FR-002-AC-2: The result is explicit and invokes nothing.
            - FR-002-CON-3: Installation help is retained exactly.
        """
        host = FixtureHost(None, INSTALL_HELP)

        result = run_handoff(payload(self.scaffold), host)

        assert result["outcome"] == "deferred"
        assert result["reason_code"] == "plugin_unavailable"
        assert result["installation_help"] == INSTALL_HELP
        assert host.invocation_count == 0

    def test_tc_010_missing_capability_and_incompatible_entry_are_distinct(
        self,
    ) -> None:
        """Description:
            Distinguish missing capability from bad metadata. TC-010, FR-002.
        Assumptions:
            - Fixtures differ in only the selected discovery condition.
        Criteria:
            - FR-002-AC-3: Stable distinct reason codes are returned.
            - IT-001-SC-06/07: Neither case invokes a fallback.
        """
        missing = self.root / "missing-capability"
        shutil.copytree(self.plugin, missing)
        shutil.rmtree(missing / "skills" / "assurance-onboarding")
        incompatible = self.root / "incompatible"
        shutil.copytree(self.plugin, incompatible)
        manifest_path = incompatible / ".codex-plugin" / "plugin.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["skills"] = "../escape"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

        missing_host = FixtureHost(missing, INSTALL_HELP, {})
        incompatible_host = FixtureHost(incompatible, INSTALL_HELP, {})
        missing_result = run_handoff(payload(self.scaffold), missing_host)
        incompatible_result = run_handoff(payload(self.scaffold), incompatible_host)

        assert missing_result["reason_code"] == "capability_missing"
        assert incompatible_result["reason_code"] == "entrypoint_incompatible"
        assert missing_result["observed_provider"] == {
            "name": "engineering-assurance",
            "version": "0.2.0",
        }
        assert incompatible_result["observed_provider"] == {
            "name": "engineering-assurance",
            "version": "0.2.0",
        }
        assert "escapes plugin root" in incompatible_result["reason"]
        assert missing_host.invocation_count == 0
        assert incompatible_host.invocation_count == 0

    def test_tc_011_independent_entry_point_mutations_fail_before_invocation(
        self,
    ) -> None:
        """Description:
            Mutate each provider entry-point dimension independently. TC-011, FR-002.
        Assumptions:
            - Each copied fixture starts from the compatible baseline.
        Criteria:
            - FR-002-AC-4: Every mutation fails before invocation.
            - IT-001-SC-08: A non-callable adapter is rejected.
        """
        mutations = {
            "identity": ("name", "other-plugin"),
            "version": ("version", "latest"),
            "skill-source": ("skills", "../outside"),
        }
        for label, (field, value) in mutations.items():
            root = self.root / label
            shutil.copytree(self.plugin, root)
            manifest_path = root / ".codex-plugin" / "plugin.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest[field] = value
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            host = FixtureHost(
                root, INSTALL_HELP, {"assurance-onboarding": lambda _: {}}
            )
            result = run_handoff(payload(self.scaffold), host)
            assert result["reason_code"] == "entrypoint_incompatible", label
            assert host.invocation_count == 0, label

        absolute_root = self.root / "absolute-skill-source"
        shutil.copytree(self.plugin, absolute_root)
        manifest_path = absolute_root / ".codex-plugin" / "plugin.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["skills"] = str((absolute_root / "skills").resolve())
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        host = FixtureHost(
            absolute_root,
            INSTALL_HELP,
            {"assurance-onboarding": lambda _: {}},
        )
        result = run_handoff(payload(self.scaffold), host)
        assert result["reason_code"] == "entrypoint_incompatible"
        assert result["reason"] == "skill source must be relative"
        assert host.invocation_count == 0

        skill_root = self.root / "skill-name"
        shutil.copytree(self.plugin, skill_root)
        skill_path = skill_root / "skills" / "assurance-onboarding" / "SKILL.md"
        skill_path.write_text(
            skill_path.read_text().replace("assurance-onboarding", "renamed")
        )
        host = FixtureHost(
            skill_root, INSTALL_HELP, {"assurance-onboarding": lambda _: {}}
        )
        result = run_handoff(payload(self.scaffold), host)
        assert result["reason_code"] == "capability_missing"
        assert host.invocation_count == 0

        host = FixtureHost(
            self.plugin, INSTALL_HELP, {"assurance-onboarding": "not-callable"}
        )
        result = run_handoff(payload(self.scaffold), host)
        assert result["reason_code"] == "entrypoint_incompatible"
        assert host.invocation_count == 0

        malformed_root = self.root / "malformed-frontmatter"
        shutil.copytree(self.plugin, malformed_root)
        malformed_skill = (
            malformed_root / "skills" / "assurance-onboarding" / "SKILL.md"
        )
        malformed_skill.write_text(
            "---\nname: assurance-onboarding\nmissing closing delimiter\n",
            encoding="utf-8",
        )
        host = FixtureHost(
            malformed_root,
            INSTALL_HELP,
            {"assurance-onboarding": lambda _: {}},
        )
        result = run_handoff(payload(self.scaffold), host)
        assert result["reason_code"] == "entrypoint_incompatible"
        assert result["reason"] == "skill frontmatter closing delimiter is missing"
        assert host.invocation_count == 0

    def test_tc_012_repository_contains_no_provider_owned_assets(self) -> None:
        """Description:
            Reject provider-owned production assets. TC-012, FR-002.
        Assumptions:
            - The minimal fixture is excluded from the production scan.
        Criteria:
            - FR-002-AC-5: No provider asset tree or skill copy exists.
            - FR-002-CON-1: Generic production paths remain ownership-clean.
        """
        assert provider_ownership_violations(REPOSITORY_ROOT) == []

        synthetic = self.root / "synthetic-ownership-violations"
        # A provider-owned policy copied into the generic skill. IT-001-SC-10
        # forbids this because the provider owns it, not because it sits in a
        # directory called policies -- this repository owns policies of its own.
        (synthetic / "new-project" / "policies").mkdir(parents=True)
        (synthetic / "new-project" / "policies" / "copied.md").write_text(
            "# Assurance onboarding\nprovider policy\n", encoding="utf-8"
        )
        # A policy the generic layer owns is not a violation.
        (synthetic / "new-project" / "policies" / "own.md").write_text(
            "# Content Rights Policy\nExternal material is prohibited.\n",
            encoding="utf-8",
        )
        (synthetic / "renamed-provider").mkdir()
        (synthetic / "renamed-provider" / "copied-skill.md").write_text(
            "# Assurance onboarding\n## Inventory before proposing\n",
            encoding="utf-8",
        )
        (synthetic / "renamed-provider" / "profile.yaml").write_text(
            "kind: AssuranceProfile\n", encoding="utf-8"
        )
        copied_fixture = (
            synthetic
            / "other-skill"
            / "tests"
            / "engineering-assurance"
            / "skills"
            / "assurance-onboarding"
            / "SKILL.md"
        )
        copied_fixture.parent.mkdir(parents=True)
        copied_fixture.write_text(
            "---\nname: assurance-onboarding\n---\n# Assurance onboarding\n",
            encoding="utf-8",
        )
        for document_root in ("spec", "plan"):
            copied_document = (
                synthetic
                / document_root
                / "vendor"
                / "engineering-assurance"
                / "skills"
                / "assurance-onboarding"
                / "SKILL.md"
            )
            copied_document.parent.mkdir(parents=True)
            copied_document.write_text(
                "---\nname: assurance-onboarding\n---\n# Assurance onboarding\n",
                encoding="utf-8",
            )

        violations = provider_ownership_violations(synthetic)
        assert len(violations) >= 4
        # A copied provider skill body, wherever it sits.
        assert any("# Assurance onboarding" in item for item in violations)
        # A written assurance artifact, matched by its structured declaration
        # rather than by the type name appearing in prose.
        assert any("written assurance artifact 'AssuranceProfile'" in item for item in violations)
        # A provider tree, including under a test or documentation path.
        assert any(
            "other-skill/tests/engineering-assurance" in item for item in violations
        )
        assert any("spec/vendor/engineering-assurance" in item for item in violations)
        assert any("plan/vendor/engineering-assurance" in item for item in violations)

        # A provider-owned policy is caught wherever it is copied to.
        assert any("policies/copied.md" in item for item in violations)
        # A policy the generic layer owns is NOT a violation. The previous
        # allowlist made every addition to new-project fail this audit, which
        # is how the rights preflight and CONTENT_RIGHTS.md turned it red.
        assert not any("policies/own.md" in item for item in violations)

    def test_tc_013_no_plugin_host_preserves_scaffold_and_explicit_outcomes(
        self,
    ) -> None:
        """Description:
            Preserve outcomes when the host has no plugin system. TC-013, FR-002.
        Assumptions:
            - The scaffold is already verified and content-addressed.
        Criteria:
            - FR-002-AC-6: Invoke defers and not-applicable remains explicit.
            - IT-001-SC-09: The scaffold remains unchanged.
        """
        before = tree_digest(self.scaffold)

        unavailable = run_handoff(payload(self.scaffold), None)
        not_applicable = run_handoff(
            payload(self.scaffold, decision="not_applicable"), None
        )

        assert unavailable["reason_code"] == "plugin_unavailable"
        assert not_applicable["outcome"] == "not_applicable"
        assert tree_digest(self.scaffold) == before

    def test_tc_014_fixture_is_minimal_and_never_resolves_real_provider(self) -> None:
        """Description:
            Prove the fixture is minimal and provider-independent. TC-014, FR-002.
        Assumptions:
            - Only the fixture manifest and capture-skill metadata are allowed.
        Criteria:
            - FR-002-AC-7: The real provider is neither vendored nor resolved.
            - FR-002-CON-2: No provider workflow or artifact behavior appears.
        """
        fixture_files = sorted(
            path.relative_to(FIXTURE).as_posix()
            for path in FIXTURE.rglob("*")
            if path.is_file()
        )
        fixture_text = "\n".join(
            path.read_text(encoding="utf-8")
            for path in FIXTURE.rglob("*")
            if path.is_file()
        )

        assert fixture_files == [
            ".codex-plugin/plugin.json",
            "skills/assurance-onboarding/SKILL.md",
        ]
        assert "engineering_assurance/skills" not in fixture_text
        assert "ix-flow" not in fixture_text.lower()
        assert "AssuranceProfile" not in fixture_text
