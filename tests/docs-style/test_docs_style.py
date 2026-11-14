from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Dict, List

import pytest

yaml = pytest.importorskip("yaml")

SKILL = Path(__file__).parents[2] / "skills" / "docs-style"
sys.path.insert(0, str(SKILL / "scripts"))
sys.dont_write_bytecode = True

import docs_lint  # noqa: E402

CATALOG = yaml.safe_load((SKILL / "rules" / "catalog.yaml").read_text(encoding="utf-8"))["rules"]
BY_ID = {r["id"]: r for r in CATALOG}
VALE = shutil.which("vale")
needs_vale = pytest.mark.skipif(VALE is None, reason="vale is not on PATH")


def pascal(rule_id: str) -> str:
    return "".join(part.capitalize() for part in rule_id.split("-"))


# ---------------------------------------------------------------- catalog


def test_catalog_entries_are_well_formed() -> None:
    ids = [r["id"] for r in CATALOG]
    assert len(ids) == len(set(ids))
    for r in CATALOG:
        assert re.fullmatch(r"[a-z][a-z0-9]*(-[a-z0-9]+)*", r["id"])
        assert r["severity"] in ("error", "warning", "suggestion")
        assert r["check"] in ("vale", "structure", "review")
        assert r.get("backstop", "structure") == "structure"
        assert set(r["applies_to"]) <= set(docs_lint.PAGE_TYPES) | {"all"}
        for key in ("rationale", "bad", "good"):
            assert r[key].strip(), (r["id"], key)


def test_line_parser_matches_yaml() -> None:
    loaded = docs_lint.load_catalog()
    assert set(loaded) == set(BY_ID)
    for rid, r in BY_ID.items():
        assert loaded[rid]["severity"] == r["severity"]
        assert loaded[rid]["check"] == r["check"]
        assert loaded[rid].get("backstop") == r.get("backstop")
        assert loaded[rid]["applies_to"] == r["applies_to"]


def test_vale_rules_match_style_files_one_to_one() -> None:
    wanted = {pascal(r["id"]) for r in CATALOG if r["check"] == "vale"}
    present = {p.stem for p in (SKILL / "vale" / "AgentIX").glob("*.yml")}
    assert wanted == present


def test_vale_levels_match_catalog() -> None:
    for r in CATALOG:
        if r["check"] != "vale":
            continue
        text = (SKILL / "vale" / "AgentIX" / (pascal(r["id"]) + ".yml")).read_text(encoding="utf-8")
        assert re.search(r"^level: %s$" % r["severity"], text, re.M), r["id"]


def test_script_rules_have_scripts() -> None:
    for p in (SKILL / "vale" / "AgentIX").glob("*.yml"):
        m = re.search(r"^script: (\S+)$", p.read_text(encoding="utf-8"), re.M)
        if m:
            assert (SKILL / "vale" / "config" / "scripts" / m.group(1)).is_file()


def test_references_and_catalog_agree() -> None:
    cited = set()
    for ref in list((SKILL / "references").glob("*.md")):
        for group in re.findall(r"\(rules?: ([^)]+)\)", ref.read_text(encoding="utf-8").replace("\n", " ")):
            cited.update(x.strip() for x in group.split(","))
    assert cited - set(BY_ID) == set(), "references cite unknown rules"
    assert set(BY_ID) - cited == set(), "catalog rules missing from references"


# ---------------------------------------------------------------- vale

LONG = ("The compiler checks that the rule names only objects fields and operations that exist in "
        "the domain model and the evaluator then runs the rule on data from the system and reports "
        "whether the system follows it.")

VALE_CASES: Dict[str, tuple] = {
    "contrast-framing": ("A clock is a rule, not a stopwatch.", "A clock gives each position a number."),
    "figurative-words": ("The honest answer is clear.", "The answer is clear."),
    "lead-ins": ("Here is the rule.", "The rule holds."),
    "page-self-reference": ("This page covers clocks.", "A clock numbers positions."),
    "hedging-stack": ("A rule may generally be reused.", "A rule is reused."),
    "sentence-too-long": (LONG, "The compiler checks the rule. The evaluator runs it."),
    "undefined-acronym": ("The SPRT stops early.",
                          "The Sequential Probability Ratio Test (SPRT) stops early. The SPRT is fast."),
    "us-spelling": ("The behaviour is recorded.", "The behavior is recorded."),
    "unconnected-systems": ("It does not depend on Quire.", "It parses formulas."),
    "question-heading": ("## Why does it work?\n\nIt works.", "## How it works\n\nIt works."),
    "maintainer-process": ("See AGENTS.md for the workflow.", "Run make test first."),
    "status-in-prose": ("The API is not stable.", "The API returns a list."),
    "requirement-id-in-prose": ("The evaluator follows FR-043.", "The evaluator follows the spec."),
    "issue-ref-in-prose": ("Fixed in quire-rs#123.", "Fixed in the next release."),
    "rust-path-in-prose": ("It reads Config::Version first.", "It reads the version first."),
    "crate-name-in-prose": ("The `qsl-semantics` crate holds it.", "The compiler holds it."),
    "source-path-in-prose": ("It lives in `qsl-semantics/src/check/profile.rs`.", "It lives in the compiler."),
    "test-name-in-prose": ("This is covered by tc_084_roundtrip.", "This round-trips."),
    "on-main-branch": ("The flag exists on main.", "The flag sets the output directory."),
    "version-pin": ("This section documents Quoin 0.29.1.", "Install the package with npm."),
    "no-command-runs": ("No `quire-spec` command runs the clause.", "The clause is compiled."),
    "banned-heading": ("## Status of this page\n\nText.", "## Usage\n\nText."),
    "table-too-wide": ("| a | b | c | d | e |\n| --- | --- | --- | --- | --- |\n| 1 | 2 | 3 | 4 | 5 |",
                       "| a | b | c | d |\n| --- | --- | --- | --- |\n| 1 | 2 | 3 | 4 |"),
    "table-code-cell-long": ("| Id | Tick |\n| --- | --- |\n| `quire.temporal.event-position.false-extension/v1` | event |",
                             "| Id | Tick |\n| --- | --- |\n| `event-position` | event |"),
}


def vale_checks(tmp_path: Path, body: str) -> List[str]:
    ini = tmp_path / "vale.ini"
    ini.write_text("StylesPath = %s\nMinAlertLevel = suggestion\n[*.md]\nBasedOnStyles = AgentIX\n" % (SKILL / "vale"))
    page = tmp_path / "page.md"
    page.write_text(body if body.endswith("\n") else body + "\n", encoding="utf-8")
    proc = subprocess.run([str(VALE), "--config", str(ini), "--output", "JSON", str(page)],
                          capture_output=True, text=True)
    assert proc.returncode in (0, 1), proc.stderr + proc.stdout
    data = json.loads(proc.stdout or "{}")
    return [a["Check"] for alerts in data.values() for a in alerts]


def test_every_vale_rule_has_a_case() -> None:
    assert {r["id"] for r in CATALOG if r["check"] == "vale"} == set(VALE_CASES)


@needs_vale
@pytest.mark.parametrize("rule_id", sorted(VALE_CASES))
def test_vale_rule_fails_and_passes(rule_id: str, tmp_path: Path) -> None:
    bad, good = VALE_CASES[rule_id]
    name = "AgentIX." + pascal(rule_id)
    assert name in vale_checks(tmp_path, "Intro text.\n\n" + bad)
    assert name not in vale_checks(tmp_path, "Intro text.\n\n" + good)


@needs_vale
@pytest.mark.parametrize("rule_id", sorted(VALE_CASES))
def test_vale_rule_skips_fenced_code(rule_id: str, tmp_path: Path) -> None:
    bad, _ = VALE_CASES[rule_id]
    if bad.startswith("## "):
        pytest.skip("headings cannot sit in a fence")
    fenced = "Intro text.\n\n```text\n%s\n```\n" % bad
    assert "AgentIX." + pascal(rule_id) not in vale_checks(tmp_path, fenced)


@needs_vale
def test_vale_ignores_inline_code_in_text_rules(tmp_path: Path) -> None:
    body = "Intro text.\n\nUse `FR-043` and `Config::Version` and `a, not b` in a command.\n"
    found = set(vale_checks(tmp_path, body))
    assert "AgentIX.RequirementIdInProse" not in found
    assert "AgentIX.RustPathInProse" not in found
    assert "AgentIX.ContrastFraming" not in found


# ---------------------------------------------------------------- structure

CONCEPT_OK = """---
title: How time works
type: concept
---

Time is the order of events.

![A trace with positions.](trace.svg)

## The problem

A rule needs a count.

## Example

A short example.

## Next steps

- [Reference](ref.md)
"""

HOWTO_OK = """---
title: Check a trace
type: how-to
---

Check a trace against a rule.

1. Run the check.
2. Read the result.

## Expected output

The check prints holds.
"""


def lint(tmp_path: Path, files: Dict[str, str], *extra: str) -> List[str]:
    paths = []
    for name, text in files.items():
        p = tmp_path / name
        p.write_text(text, encoding="utf-8")
        paths.append(str(p))
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), "--level", "suggestion", *extra, *paths],
                          capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    return [line.split(": ")[1] for line in proc.stdout.splitlines()]


STRUCT_CASES = {
    "page-type-declared": ({"a.md": "---\ntitle: A\n---\n\nText.\n"}, {"a.md": "---\ntitle: A\ntype: reference\n---\n\nText.\n"}),
    "required-sections": ({"a.md": CONCEPT_OK.replace("## Next steps", "## More")}, {"a.md": CONCEPT_OK}),
    "page-length-budget": ({"a.md": HOWTO_OK + "word " * 800}, {"a.md": HOWTO_OK}),
    "heading-level-skip": ({"a.md": CONCEPT_OK.replace("## The problem", "#### The problem")}, {"a.md": CONCEPT_OK}),
    "duplicate-title": ({"a.md": CONCEPT_OK, "b.md": CONCEPT_OK.replace("type: concept", "type: concept")},
                        {"a.md": CONCEPT_OK, "b.md": CONCEPT_OK.replace("How time works", "How state works")}),
    "near-duplicate-topic": ({"a.md": CONCEPT_OK, "b.md": CONCEPT_OK.replace("How time works", "Time profiles works")},
                             {"a.md": CONCEPT_OK, "b.md": "---\ntitle: Probability claims\ntype: concept\n---\n\nText.\n\n## Claims\n"}),
    "table-too-wide": ({"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b | c | d | e |\n| --- | --- | --- | --- | --- |\n| 1 | 2 | 3 | 4 | 5 |\n"},
                       {"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n"}),
    "table-code-cell-long": ({"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n| `" + "x" * 40 + "` | 2 |\n"},
                             {"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n| `x` | 2 |\n"}),
    "table-cell-too-long": ({"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n| " + "word " * 14 + "| 2 |\n"},
                            {"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n| short | 2 |\n"}),
    "table-without-intro": ({"a.md": CONCEPT_OK + "\n## Table\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n"},
                            {"a.md": CONCEPT_OK + "\n## Table\n\nThe table lists pairs.\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n"}),
    "table-too-long": ({"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n" + "| 1 | 2 |\n" * 15},
                       {"a.md": CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n" + "| 1 | 2 |\n" * 3}),
}


def test_every_structure_rule_has_a_case() -> None:
    structural = {r["id"] for r in CATALOG if r["check"] == "structure" or r.get("backstop") == "structure"}
    assert structural == set(STRUCT_CASES)


@pytest.mark.parametrize("rule_id", sorted(STRUCT_CASES))
def test_structure_rule_fails_and_passes(rule_id: str, tmp_path: Path) -> None:
    bad, good = STRUCT_CASES[rule_id]
    assert rule_id in lint(_sub(tmp_path, "bad"), bad)
    assert rule_id not in lint(_sub(tmp_path, "good"), good)


def _sub(tmp_path: Path, name: str) -> Path:
    d = tmp_path / name
    d.mkdir(exist_ok=True)
    return d


@pytest.mark.parametrize("kind", ["concept", "tutorial", "how-to", "reference", "status", "readme"])
def test_required_sections_per_type(kind: str, tmp_path: Path) -> None:
    empty = "---\ntitle: T\ntype: %s\n---\n\nText.\n" % kind
    assert "required-sections" in lint(tmp_path, {"a.md": empty})


def test_how_to_passes_and_type_flag_works(tmp_path: Path) -> None:
    assert lint(tmp_path, {"a.md": HOWTO_OK}) == []
    assert "page-type-declared" not in lint(_sub(tmp_path, "t"), {"a.md": "Text.\n"}, "--type", "reference")


def test_emitted_rule_ids_are_in_the_catalog(tmp_path: Path) -> None:
    emitted = set()
    for bad, _ in STRUCT_CASES.values():
        d = _sub(tmp_path, "e%d" % len(emitted))
        emitted.update(lint(d, bad))
    assert emitted <= set(BY_ID)
    for rid in emitted:
        assert BY_ID[rid]["check"] == "structure" or BY_ID[rid].get("backstop") == "structure"


def test_check_flag_sets_exit_status(tmp_path: Path) -> None:
    f = tmp_path / "a.md"
    f.write_text("Text.\n", encoding="utf-8")
    script = str(SKILL / "scripts" / "docs_lint.py")
    assert subprocess.run([sys.executable, script, str(f)], capture_output=True).returncode == 0
    assert subprocess.run([sys.executable, script, "--check", str(f)], capture_output=True).returncode == 1
    f.write_text(HOWTO_OK, encoding="utf-8")
    assert subprocess.run([sys.executable, script, "--check", str(f)], capture_output=True).returncode == 0


# ---------------------------------------------------------------- calibration

OFFENDERS = Path(__file__).parent / "fixtures" / "offenders"
EXPECTED = {
    "profiles.md": {"AgentIX.RequirementIdInProse", "AgentIX.TableTooWide", "AgentIX.TableCodeCellLong",
                    "AgentIX.NoCommandRuns", "AgentIX.PageSelfReference"},
    "probability-index.md": {"AgentIX.StatusInProse", "AgentIX.CrateNameInProse", "AgentIX.UsSpelling"},
    "clocks-and-positions.md": {"AgentIX.RequirementIdInProse", "AgentIX.TestNameInProse", "AgentIX.SourcePathInProse"},
    "quoin-index.md": {"AgentIX.VersionPin", "AgentIX.PageSelfReference", "AgentIX.TableCodeCellLong"},
}


@needs_vale
@pytest.mark.parametrize("name", sorted(EXPECTED))
def test_offender_excerpts_fail_vale(name: str, tmp_path: Path) -> None:
    ini = tmp_path / "vale.ini"
    ini.write_text("StylesPath = %s\nMinAlertLevel = warning\n[*.md]\nBasedOnStyles = AgentIX\n" % (SKILL / "vale"))
    proc = subprocess.run([str(VALE), "--config", str(ini), "--output", "JSON", str(OFFENDERS / name)],
                          capture_output=True, text=True)
    assert proc.returncode == 1
    found = {a["Check"] for alerts in json.loads(proc.stdout).values() for a in alerts}
    assert EXPECTED[name] <= found, EXPECTED[name] - found


@pytest.mark.parametrize("name", sorted(EXPECTED))
def test_offender_excerpts_fail_structure(name: str) -> None:
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), "--check", "--type", "concept",
                           str(OFFENDERS / name)], capture_output=True, text=True)
    assert proc.returncode == 1
    assert "required-sections" in proc.stdout
