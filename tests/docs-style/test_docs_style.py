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
    "figurative-words": ("The foundation sits under the model.", "The model is the input."),
    "trust-words": ("The honest answer is clear.", "The answer is clear."),
    "lead-ins": ("Here is the rule.", "The rule holds."),
    "page-self-reference": ("This page covers clocks.", "A clock numbers positions."),
    "hedging-stack": ("A rule may generally be reused.", "A rule is reused."),
    "sentence-too-long": (LONG, "The compiler checks the rule. The evaluator runs it."),
    "undefined-acronym": ("The SPRT stops early.",
                          "The sequential probability ratio test (SPRT) stops early. The SPRT is fast."),
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


DESC_PAGE_STRUCT = CONCEPT_OK.replace("type: concept\n", "type: concept\ndescription: %s\n")

STRUCT_CASES = {
    "page-type-declared": ({"a.md": "---\ntitle: A\n---\n\nText.\n"}, {"a.md": "---\ntitle: A\ntype: reference\n---\n\nText.\n"}),
    "required-sections": ({"a.md": CONCEPT_OK.replace("## Next steps", "## More")}, {"a.md": CONCEPT_OK}),
    "page-length-budget": ({"a.md": HOWTO_OK + "word " * 800}, {"a.md": HOWTO_OK}),
    "heading-level-skip": ({"a.md": CONCEPT_OK.replace("## The problem", "#### The problem")}, {"a.md": CONCEPT_OK}),
    "duplicate-title": ({"a.md": CONCEPT_OK, "b.md": CONCEPT_OK},
                        {"a.md": CONCEPT_OK, "b.md": CONCEPT_OK.replace("How time works", "How state works")}),
    "near-duplicate-topic": ({"a.md": CONCEPT_OK, "b.md": CONCEPT_OK.replace("How time works", "Time works")},
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
    "page-self-reference": ({"a.md": DESC_PAGE_STRUCT % "This page covers clocks."}, {"a.md": DESC_PAGE_STRUCT % "A clock numbers positions."}),
    "lead-ins": ({"a.md": DESC_PAGE_STRUCT % "Here is how time works."}, {"a.md": DESC_PAGE_STRUCT % "Time orders events."}),
    "contrast-framing": ({"a.md": DESC_PAGE_STRUCT % "A clock is a rule, not a stopwatch."}, {"a.md": DESC_PAGE_STRUCT % "A clock numbers positions."}),
    "sentence-too-long": ({"a.md": DESC_PAGE_STRUCT % LONG}, {"a.md": DESC_PAGE_STRUCT % "The compiler checks the rule."}),
    "concept-no-harness": ({"a.md": CONCEPT_OK + "\n```bash\ncheck rule.native trace.json | jq .result\n```\n"},
                           {"a.md": CONCEPT_OK + "\n```bash\ncheck rule.native trace.json\n```\n"}),
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
    "profiles.md": {"AgentIX.RequirementIdInProse", "AgentIX.TableCodeCellLong", "AgentIX.NoCommandRuns",
                    "AgentIX.PageSelfReference", "AgentIX.BannedHeading", "AgentIX.SourcePathInProse",
                    "AgentIX.CrateNameInProse", "AgentIX.TestNameInProse"},
    "probability-index.md": {"AgentIX.StatusInProse", "AgentIX.CrateNameInProse", "AgentIX.UsSpelling",
                             "AgentIX.TrustWords", "AgentIX.SentenceTooLong"},
    "clocks-and-positions.md": {"AgentIX.RequirementIdInProse", "AgentIX.TestNameInProse", "AgentIX.SourcePathInProse",
                                "AgentIX.RustPathInProse", "AgentIX.PageSelfReference"},
    "quoin-index.md": {"AgentIX.VersionPin", "AgentIX.PageSelfReference",
                       "AgentIX.LeadIns", "AgentIX.ContrastFraming", "AgentIX.OnMainBranch", "AgentIX.FigurativeWords"},
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


# ---------------------------------------------------------------- vale: masks and tuning


def alerts(tmp_path: Path, body: str) -> List[dict]:
    ini = tmp_path / "vale.ini"
    ini.write_text("StylesPath = %s\nMinAlertLevel = suggestion\n[*.md]\nBasedOnStyles = AgentIX\n" % (SKILL / "vale"))
    page = tmp_path / "page.md"
    page.write_text(body, encoding="utf-8")
    proc = subprocess.run([str(VALE), "--config", str(ini), "--output", "JSON", str(page)], capture_output=True, text=True)
    assert proc.returncode in (0, 1), proc.stderr + proc.stdout
    return [a for v in json.loads(proc.stdout or "{}").values() for a in v]


def names(tmp_path: Path, body: str) -> List[str]:
    return [a["Check"] for a in alerts(tmp_path, body)]


FIVE = "| a | b | c | d | e |\n| --- | --- | --- | --- | --- |\n| 1 | 2 | 3 | 4 | 5 |\n"

MASKED = {
    "frontmatter": ("---\nrepo: qsl-semantics\n---\n\nText.\n", "AgentIX.CrateNameInProse"),
    "link-target": ("Text.\n\n[the repo](https://github.com/agent-ix/qsl-semantics) is public.\n", "AgentIX.CrateNameInProse"),
    "bare-url": ("Text.\n\nSee https://example.com/qsl-semantics for more.\n", "AgentIX.CrateNameInProse"),
    "html-comment": ("Text.\n\n<!-- qsl-comment -->\n", "AgentIX.CrateNameInProse"),
    "indented-code": ("Text.\n\n    " + FIVE.replace("\n", "\n    ") + "\nMore.\n", "AgentIX.TableTooWide"),
    "nested-fence": ("Text.\n\n````markdown\n```bash\nNo `x` command runs.\n```\nNo `x` command runs.\n````\n", "AgentIX.NoCommandRuns"),
    "tilde-fence": ("Text.\n\n~~~\n```\nqsl-semantics\n~~~\n", "AgentIX.CrateNameInProse"),
    "short-closer": ("Text.\n\n````\nqsl-semantics\n```\nqsl-semantics\n````\n", "AgentIX.CrateNameInProse"),
    "indented-fence": ("- item\n\n  ```text\n  qsl-semantics\n  ```\n", "AgentIX.CrateNameInProse"),
}


@needs_vale
@pytest.mark.parametrize("case", sorted(MASKED))
def test_script_rules_skip_masked_regions(case: str, tmp_path: Path) -> None:
    body, rule = MASKED[case]
    assert rule not in names(tmp_path, body)


@needs_vale
def test_script_rules_still_fire_after_a_fence(tmp_path: Path) -> None:
    body = "Text.\n\n```text\nqsl-semantics\n```\n\nThe `qsl-semantics` crate.\n"
    assert names(tmp_path, body).count("AgentIX.CrateNameInProse") == 1


@needs_vale
def test_crate_name_gives_one_alert_per_occurrence(tmp_path: Path) -> None:
    assert names(tmp_path, "Text.\n\nThe `qsl-semantics` crate holds it.\n").count("AgentIX.CrateNameInProse") == 1


@needs_vale
def test_pipeless_and_single_dash_tables(tmp_path: Path) -> None:
    pipeless = "Text.\n\na | b | c | d | e\n- | - | - | - | -\n1 | 2 | 3 | 4 | 5\n"
    single = "Text.\n\n|a|b|c|d|e|\n|-|-|-|-|-|\n|1|2|3|4|5|\n"
    assert "AgentIX.TableTooWide" in names(tmp_path, pipeless)
    assert "AgentIX.TableTooWide" in names(_sub(tmp_path, "s"), single)
    four = "Text.\n\na | b | c | d\n- | - | - | -\n1 | 2 | 3 | 4\n"
    assert "AgentIX.TableTooWide" not in names(_sub(tmp_path, "f"), four)


@needs_vale
def test_vale_and_docs_lint_agree_on_table_lines(tmp_path: Path) -> None:
    body = "---\ntitle: T\ntype: concept\n---\n\nThe table:\n\n" + FIVE + "\nAnother:\n\n| a | b |\n| - | - |\n| `" + "x" * 40 + "` | 2 |\n"
    vale_lines = {a["Check"]: a["Line"] for a in alerts(tmp_path, body)}
    out = lint_lines(_sub(tmp_path, "d"), {"a.md": body})
    assert vale_lines["AgentIX.TableTooWide"] == out["table-too-wide"]
    assert vale_lines["AgentIX.TableCodeCellLong"] == out["table-code-cell-long"]


def lint_lines(tmp_path: Path, files: Dict[str, str]) -> Dict[str, int]:
    for name, text in files.items():
        (tmp_path / name).write_text(text, encoding="utf-8")
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), "--level", "suggestion", str(tmp_path / name)],
                          capture_output=True, text=True)
    out: Dict[str, int] = {}
    for line in proc.stdout.splitlines():
        _, ln, rule, _ = line.split(":", 3)
        out.setdefault(rule.strip(), int(ln))
    return out


TUNING = [
    ("The foundation holds.", "AgentIX.FigurativeWords", True),
    ("It stands in the way.", "AgentIX.FigurativeWords", True),
    ("The sum lies in the range.", "AgentIX.TrustWords", False),
    ("It lies to the caller.", "AgentIX.TrustWords", True),
    ("The binary stands alone.", "AgentIX.FigurativeWords", False),
    ("See STD-123 and V1-TEMP-021.", "AgentIX.RequirementIdInProse", True),
    ("SHA-256 and UTF-8 are formats.", "AgentIX.RequirementIdInProse", False),
    ("It runs on the main thread.", "AgentIX.OnMainBranch", False),
    ("The flag exists on main.", "AgentIX.OnMainBranch", True),
    ("Run `cargo test --doc` to check it.", "AgentIX.TestNameInProse", False),
    ("This mirrors `tests/reference.rs`.", "AgentIX.TestNameInProse", True),
    ("The engine is specified but not built.", "AgentIX.StatusInProse", True),
    ("The sequential probability ratio test (SPRT) is fast. SPRT stops early.", "AgentIX.UndefinedAcronym", False),
    ("Read CONTRIBUTING.md and CLAUDE.md first.", "AgentIX.UndefinedAcronym", False),
]


@needs_vale
@pytest.mark.parametrize("text,check,expected", TUNING)
def test_tuned_rules(text: str, check: str, expected: bool, tmp_path: Path) -> None:
    assert (check in names(tmp_path, "Intro text.\n\n" + text + "\n")) is expected


@needs_vale
def test_inline_suppression_comment(tmp_path: Path) -> None:
    body = "Intro text.\n\n<!-- vale AgentIX.LeadIns = NO -->\nHere is the rule.\n<!-- vale AgentIX.LeadIns = YES -->\n\nHere is the rule.\n"
    found = [a for a in alerts(tmp_path, body) if a["Check"] == "AgentIX.LeadIns"]
    assert [a["Line"] for a in found] == [7]


@needs_vale
@pytest.mark.skipif(shutil.which("mdx2vast") is None, reason="mdx2vast is not on PATH")
def test_mdx_is_linted(tmp_path: Path) -> None:
    ini = tmp_path / "vale.ini"
    ini.write_text("StylesPath = %s\n[*.mdx]\nBasedOnStyles = AgentIX\n" % (SKILL / "vale"))
    page = tmp_path / "a.mdx"
    page.write_text("Intro text.\n\nHere is the rule.\n", encoding="utf-8")
    proc = subprocess.run([str(VALE), "--config", str(ini), "--output", "JSON", str(page)], capture_output=True, text=True)
    assert "AgentIX.LeadIns" in proc.stdout


@needs_vale
def test_per_glob_section_turns_a_rule_off(tmp_path: Path) -> None:
    (tmp_path / "ref").mkdir()
    page = tmp_path / "ref" / "a.md"
    page.write_text("Intro text.\n\nThe `qsl-semantics` crate.\n", encoding="utf-8")
    ini = tmp_path / "vale.ini"
    ini.write_text("StylesPath = %s\n[*.md]\nBasedOnStyles = AgentIX\n[**/ref/*.md]\nAgentIX.CrateNameInProse = NO\n" % (SKILL / "vale"))
    proc = subprocess.run([str(VALE), "--config", str(ini), "--output", "JSON", str(page)], capture_output=True, text=True)
    assert "CrateNameInProse" not in proc.stdout


# ---------------------------------------------------------------- docs_lint: tuning


def test_nested_fences_do_not_leak(tmp_path: Path) -> None:
    body = "---\ntitle: T\ntype: reference\n---\n\n## Syntax\n\n````markdown\n```bash\n# install\n" + FIVE + "```\n````\n\n### Forms\n"
    found = lint(tmp_path, {"a.md": body})
    assert "table-too-wide" not in found and "heading-level-skip" not in found and "table-without-intro" not in found


def test_tilde_fence_is_not_closed_by_backticks(tmp_path: Path) -> None:
    body = "---\ntitle: T\ntype: reference\n---\n\n~~~\n```\n" + FIVE + "~~~\n"
    assert "table-too-wide" not in lint(tmp_path, {"a.md": body})


def test_pipeless_table_is_checked(tmp_path: Path) -> None:
    body = CONCEPT_OK + "\nThe table:\n\na | b | c | d | e\n- | - | - | - | -\n1 | 2 | 3 | 4 | 5\n"
    assert "table-too-wide" in lint(tmp_path, {"a.md": body})


def test_type_flag_does_not_override_frontmatter(tmp_path: Path) -> None:
    findings = lint(tmp_path, {"a.md": CONCEPT_OK}, "--type", "how-to")
    assert "required-sections" not in findings


def test_bom_and_crlf_frontmatter(tmp_path: Path) -> None:
    f = tmp_path / "a.md"
    f.write_bytes(("\ufeff" + CONCEPT_OK.replace("\n", "\r\n")).encode("utf-8"))
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), "--level", "suggestion", str(f)], capture_output=True, text=True)
    assert proc.stdout == "" and proc.returncode == 0


def test_bad_input_does_not_crash(tmp_path: Path) -> None:
    script = str(SKILL / "scripts" / "docs_lint.py")
    missing = subprocess.run([sys.executable, script, str(tmp_path / "nope.md")], capture_output=True, text=True)
    assert missing.returncode == 2 and "Traceback" not in missing.stderr and "internal" in missing.stdout
    binary = tmp_path / "b.md"
    binary.write_bytes(b"\xff\xfe\x00bad")
    bad = subprocess.run([sys.executable, script, str(binary)], capture_output=True, text=True)
    assert bad.returncode == 2 and "Traceback" not in bad.stderr
    empty = tmp_path / "e.md"
    empty.write_text("")
    assert subprocess.run([sys.executable, script, "--type", "reference", str(empty)], capture_output=True).returncode == 0


def test_readme_is_detected_by_name_at_the_root_only(tmp_path: Path) -> None:
    (tmp_path / "docs" / "other").mkdir(parents=True)
    (tmp_path / "README.md").write_text("Text.\n", encoding="utf-8")
    (tmp_path / "docs" / "other" / "readme.md").write_text("Text.\n", encoding="utf-8")
    (tmp_path / "docs" / "README.md").write_text("Text.\n", encoding="utf-8")
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), str(tmp_path)], capture_output=True, text=True)
    lines = proc.stdout.splitlines()
    assert any("README.md" in l and "required-sections" in l and "/docs/" not in l for l in lines)
    assert any("other/readme.md" in l and "page-type-declared" in l for l in lines)
    assert any("docs/README.md" in l and "page-type-declared" in l for l in lines)


def test_examples_heading_counts(tmp_path: Path) -> None:
    assert "required-sections" not in lint(tmp_path, {"a.md": CONCEPT_OK.replace("## Example", "## Examples")})


def test_well_formed_pages_are_not_near_duplicates(tmp_path: Path) -> None:
    other = CONCEPT_OK.replace("How time works", "Probability claims").replace("A rule needs a count.", "A claim has a threshold.")
    assert "near-duplicate-topic" not in lint(tmp_path, {"a.md": CONCEPT_OK, "b.md": other})
    tut = "---\ntitle: {}\ntype: tutorial\n---\n\nText.\n\n## Before you start\n\n## Step 1\n\n## Step 2\n\n## Next steps\n"
    assert "near-duplicate-topic" not in lint(_sub(tmp_path, "t"), {"a.md": tut.format("Check traces"), "b.md": tut.format("Write rules")})


def test_subset_titles_are_not_duplicates(tmp_path: Path) -> None:
    a = "---\ntitle: Model checking\ntype: reference\n---\n"
    b = "---\ntitle: Statistical model checking\ntype: reference\n---\n"
    assert "near-duplicate-topic" not in lint(tmp_path, {"a.md": a, "b.md": b})


def test_applies_to_filters_structure_rules(tmp_path: Path) -> None:
    long_table = CONCEPT_OK + "\nThe table:\n\n| a | b |\n| --- | --- |\n" + "| 1 | 2 |\n" * 15
    assert "table-too-long" in lint(tmp_path, {"a.md": long_table})
    ref = long_table.replace("type: concept", "type: reference")
    assert "table-too-long" not in lint(_sub(tmp_path, "r"), {"a.md": ref})
    cat = tmp_path / "cat.yaml"
    cat.write_text(Path(SKILL / "rules" / "catalog.yaml").read_text(encoding="utf-8").replace(
        "id: table-too-wide\n    severity: error\n    check: vale\n    backstop: structure\n    applies_to: [all]",
        "id: table-too-wide\n    severity: error\n    check: vale\n    backstop: structure\n    applies_to: [reference]"))
    wide = CONCEPT_OK + "\nThe table:\n\n" + FIVE
    d = _sub(tmp_path, "c")
    (d / "a.md").write_text(wide, encoding="utf-8")
    script = str(SKILL / "scripts" / "docs_lint.py")
    plain = subprocess.run([sys.executable, script, str(d / "a.md")], capture_output=True, text=True).stdout
    scoped = subprocess.run([sys.executable, script, "--catalog", str(cat), str(d / "a.md")], capture_output=True, text=True).stdout
    assert "table-too-wide" in plain and "table-too-wide" not in scoped


def test_vale_sections_match_the_sample_ini() -> None:
    out = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), "--vale-sections", "reference=docs/reference/**/*.md"],
                         capture_output=True, text=True).stdout.strip().splitlines()
    commented = [l[2:] for l in (SKILL / "vale" / ".vale.ini").read_text(encoding="utf-8").splitlines() if l.startswith("# ")]
    for line in out:
        assert line in commented
    for r in CATALOG:
        if r["check"] == "vale" and "all" not in r["applies_to"] and "reference" not in r["applies_to"]:
            assert "AgentIX.%s = NO" % pascal(r["id"]) in out


def test_generated_scripts_are_current() -> None:
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "gen_vale_scripts.py"), "--check"], capture_output=True, text=True)
    assert proc.returncode == 0, proc.stdout


# ---------------------------------------------------------------- the guide lints clean


GUIDE = [SKILL / "SKILL.md"] + sorted((SKILL / "references").glob("*.md"))


@needs_vale
@pytest.mark.parametrize("path", GUIDE, ids=lambda p: p.name)
def test_guide_passes_vale(path: Path, tmp_path: Path) -> None:
    ini = tmp_path / "vale.ini"
    ini.write_text("StylesPath = %s\nMinAlertLevel = suggestion\n[*.md]\nBasedOnStyles = AgentIX\n" % (SKILL / "vale"))
    proc = subprocess.run([str(VALE), "--config", str(ini), "--output", "line", str(path)], capture_output=True, text=True)
    assert proc.stdout == "", proc.stdout


@pytest.mark.parametrize("path", GUIDE, ids=lambda p: p.name)
def test_guide_passes_structure_checks(path: Path) -> None:
    proc = subprocess.run([sys.executable, str(SKILL / "scripts" / "docs_lint.py"), "--level", "suggestion", "--type", "reference",
                           "--ignore", "required-sections,page-length-budget", str(path)], capture_output=True, text=True)
    assert proc.stdout == "", proc.stdout


def test_near_duplicate_by_shared_topic_headings(tmp_path: Path) -> None:
    def page(title: str) -> str:
        return "---\ntitle: %s\ntype: concept\n---\n\nText.\n\n## Clocks\n\n## Positions\n\n## Anchors\n" % title
    assert "near-duplicate-topic" in lint(tmp_path, {"a.md": page("Alpha"), "b.md": page("Beta")})


# ---------------------------------------------------------------- word count, description, harness

FIG_PAIR = ('<figure>\n<img src="a-light.svg" alt="A trace of five events with one interval marked." class="light">\n'
            '<img src="a-dark.svg" alt="A trace of five events with one interval marked." class="dark">\n</figure>\n')


def words_of(tmp_path: Path, body: str) -> int:
    f = tmp_path / "w.md"
    f.write_text("---\ntitle: T\ntype: concept\n---\n\n" + body, encoding="utf-8")
    return docs_lint.Page(f, None).prose_words()


def test_word_count_strips_html_and_counts_alt_once(tmp_path: Path) -> None:
    alt_words = len("A trace of five events with one interval marked.".split())
    assert words_of(tmp_path, "One two three.\n\n" + FIG_PAIR) == 3 + alt_words
    assert words_of(tmp_path, "One two three.\n") == 3


def test_word_count_handles_wrappers_svg_and_markdown_images(tmp_path: Path) -> None:
    body = ('One two.\n\n<div class="wide">\n<picture>\n<source srcset="d.svg" media="(prefers-color-scheme: dark)">\n'
            '<img src="l.svg"\n     alt="Two words">\n</picture>\n</div>\n<svg viewBox="0 0 1 1"><text>many many many</text></svg>\n'
            '![Two words](x.svg)\n')
    assert words_of(tmp_path, body) == 2 + 2


def test_word_count_keeps_angle_brackets_in_code_spans(tmp_path: Path) -> None:
    assert words_of(tmp_path, "A `Vec<T>` holds items of `T> x`.\n") == 7


def test_figure_markup_no_longer_trips_the_budget(tmp_path: Path) -> None:
    body = CONCEPT_OK + FIG_PAIR * 40
    assert "page-length-budget" not in lint(tmp_path, {"a.md": body})


DESC_PAGE = CONCEPT_OK.replace("type: concept\n", "type: concept\ndescription: %s\n")


@pytest.mark.parametrize("rule_id,text", [
    ("page-self-reference", "This page covers how time works."),
    ("lead-ins", "Here is how time works."),
    ("contrast-framing", "A clock is a rule, not a stopwatch."),
    ("sentence-too-long", LONG),
])
def test_description_is_checked(rule_id: str, text: str, tmp_path: Path) -> None:
    assert rule_id in lint(tmp_path, {"a.md": DESC_PAGE % text})
    assert rule_id not in lint(_sub(tmp_path, "ok"), {"a.md": DESC_PAGE % "A clock numbers each position of a trace."})


def test_description_finding_points_at_its_line(tmp_path: Path) -> None:
    out = lint_lines(tmp_path, {"a.md": DESC_PAGE % "This page covers clocks."})
    assert out["page-self-reference"] == 4


def test_description_patterns_match_the_vale_tokens() -> None:
    for rule_id, patterns in docs_lint.DESCRIPTION_RULES.items():
        data = yaml.safe_load((SKILL / "vale" / "AgentIX" / (pascal(rule_id) + ".yml")).read_text(encoding="utf-8"))
        assert patterns == [t.replace("[’'']", "[’']") for t in data["tokens"]] or \
            [p.replace("\\u2019", "’") for p in patterns] == data["tokens"], rule_id


HARNESS_BAD = {
    "jq": "check rule.native trace.json | jq '.result'",
    "redirect": "check rule.native trace.json > out.json",
    "exit": 'check rule.native trace.json; echo "exit $?"',
    "multi-line": "check rule.native trace.json \\\n  | jq -r '.a\n  | .b'",
    "prompted": "$ check rule.native trace.json | jq .result\nviolated",
}


@pytest.mark.parametrize("kind", sorted(HARNESS_BAD))
def test_harness_is_flagged_on_teaching_pages(kind: str, tmp_path: Path) -> None:
    body = CONCEPT_OK + "\n```bash\n%s\n```\n" % HARNESS_BAD[kind]
    assert "concept-no-harness" in lint(tmp_path, {"a.md": body})
    tutorial = "---\ntitle: T\ntype: tutorial\n---\n\nIn this tutorial you check.\n\n## Before you start\n\nText.\n\n## Step 1: A\n\n```sh\n%s\n```\n\n## Step 2: B\n\nText.\n\n## Next steps\n\nText.\n" % HARNESS_BAD[kind]
    assert "concept-no-harness" in lint(_sub(tmp_path, "t"), {"a.md": tutorial})


def test_harness_allows_plain_commands_and_other_page_types(tmp_path: Path) -> None:
    plain = CONCEPT_OK + "\n```bash\ncheck rule.native trace.json 2>&1\ncheck a \\\n  --trace b.json\n```\n\n```text\nviolated | at position 3 > 2\n```\n"
    assert "concept-no-harness" not in lint(tmp_path, {"a.md": plain})
    howto = HOWTO_OK + "\n```bash\ncheck rule.native trace.json | jq .result\n```\n"
    assert "concept-no-harness" not in lint(_sub(tmp_path, "h"), {"a.md": howto})
