#!/usr/bin/env python3
"""Structure checks for Markdown docs that Vale cannot express.

Usage:
    docs_lint.py [--check] [--type TYPE] [--level LEVEL] [--ignore IDS] PATH...
    docs_lint.py --vale-sections TYPE=GLOB[,TYPE=GLOB...]

PATH is a Markdown file or a directory (searched for .md and .mdx files).
A `type:` in frontmatter wins; --type fills in for files without one. A file
named README.md given directly, or at the top of a directory argument, is a
readme.
Every finding is one line: ``path:line: rule-id: message``. Rule ids come from
``rules/catalog.yaml``. With ``--check`` the exit status is 1 when anything is
reported. Standard library only.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Dict, List, NamedTuple, Optional, Tuple

CATALOG = Path(__file__).resolve().parent.parent / "rules" / "catalog.yaml"
PAGE_TYPES = ("concept", "tutorial", "how-to", "reference", "status", "readme")
LEVELS = ("suggestion", "warning", "error")

# Words in prose (outside code fences and frontmatter) per page type.
LENGTH_BUDGET = {
    "concept": 900,
    "tutorial": 1800,
    "how-to": 700,
    "reference": 2500,
    "status": 3000,
    "readme": 1200,
}
MAX_TABLE_COLUMNS = 4
MAX_CELL_WORDS = 12
MAX_CODE_CELL_CHARS = 29
MAX_TABLE_ROWS = 12

PAGE_TYPE_SET = frozenset(PAGE_TYPES)
FIGURE_RE = re.compile(r"!\[|<img\b|<picture\b|<figure\b|<svg\b", re.I)
HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
FENCE_OPEN_RE = re.compile(r"^[ \t]*(`{3,}|~{3,})(.*)$")
FENCE_CLOSE_RE = re.compile(r"^[ \t]*(`{3,}|~{3,})[ \t]*$")
DELIM_RE = re.compile(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$")
ORDERED_RE = re.compile(r"^\s*\d+[.)]\s+\S")
STOPWORDS = {"the", "a", "an", "of", "and", "how", "to", "in", "for", "is", "what", "with", "on"}

# Required sections per page type: (description, test over the list of headings).
REQUIRED = {
    "concept": [
        ("an 'Example' section", lambda hs: any(re.search(r"\bexamples?\b", h, re.I) for h in hs)),
        ("a 'Next steps' section", lambda hs: any(re.fullmatch(r"(next steps|where to go next)", h, re.I) for h in hs)),
    ],
    "tutorial": [
        ("a 'Before you start' section", lambda hs: any(re.fullmatch(r"before you start", h, re.I) for h in hs)),
        ("at least two 'Step N' sections", lambda hs: sum(bool(re.match(r"step \d+", h, re.I)) for h in hs) >= 2),
        ("a 'Next steps' section", lambda hs: any(re.fullmatch(r"next steps", h, re.I) for h in hs)),
    ],
    "how-to": [
        ("an 'Expected output' or 'Result' section", lambda hs: any(re.fullmatch(r"(expected output|result)", h, re.I) for h in hs)),
    ],
    "reference": [
        ("a Syntax, Usage, Options or Fields section", lambda hs: any(re.fullmatch(r"(syntax|usage|options|fields)", h, re.I) for h in hs)),
        ("an Example or Examples section", lambda hs: any(re.fullmatch(r"examples?", h, re.I) for h in hs)),
    ],
    "status": [
        ("a 'What runs today' section", lambda hs: any(re.fullmatch(r"what runs today", h, re.I) for h in hs)),
    ],
    "readme": [
        ("a 'Getting started' section", lambda hs: any(re.fullmatch(r"getting started", h, re.I) for h in hs)),
        ("a 'Usage' section", lambda hs: any(re.fullmatch(r"usage", h, re.I) for h in hs)),
        ("a 'License' section", lambda hs: any(re.fullmatch(r"licen[sc]e", h, re.I) for h in hs)),
    ],
}


# Headings a page type requires or a template repeats; they say nothing about the topic.
BOILERPLATE_HEADING_RE = re.compile(
    r"(examples?|next steps|where to go next|before you start|step \d+.*|expected output|result|"
    r"syntax|usage|options|fields|getting started|licen[sc]e|what runs today|the problem)", re.I)


class Finding(NamedTuple):
    path: str
    line: int
    rule: str
    message: str


def load_catalog(path: Path = CATALOG) -> Dict[str, Dict[str, object]]:
    """Read id, severity, check, backstop and applies_to from catalog.yaml."""
    rules: Dict[str, Dict[str, object]] = {}
    current: Optional[Dict[str, object]] = None
    for raw in path.read_text(encoding="utf-8").splitlines():
        m = re.match(r"^\s+- id:\s*(\S+)\s*$", raw)
        if m:
            current = {"id": m.group(1)}
            rules[m.group(1)] = current
            continue
        m = re.match(r"^\s{4}(severity|check|backstop|applies_to):\s*(.+?)\s*$", raw)
        if m and current is not None:
            key, val = m.groups()
            if key == "applies_to":
                val = [v.strip() for v in val.strip("[]").split(",")]  # type: ignore[assignment]
            current[key] = val
    return rules


class Page:
    """A Markdown file split into frontmatter, prose lines, headings and tables."""

    def __init__(self, path: Path, forced_type: Optional[str], readme_ok: bool = False) -> None:
        self.path = path
        self.lines = path.read_text(encoding="utf-8-sig").splitlines()
        self.frontmatter: Dict[str, str] = {}
        self.body_start = 0
        self._split_frontmatter()
        self.in_fence = self._fence_mask()
        self.headings: List[Tuple[int, int, str]] = []  # (line no, level, text)
        for i in range(self.body_start, len(self.lines)):
            if self.in_fence[i]:
                continue
            m = HEADING_RE.match(self.lines[i])
            if m:
                self.headings.append((i + 1, len(m.group(1)), m.group(2)))
        self.type = self.frontmatter.get("type") or forced_type
        if not self.type and readme_ok and path.name == "README.md":
            self.type = "readme"

    def _split_frontmatter(self) -> None:
        if self.lines and self.lines[0].strip() == "---":
            for i in range(1, len(self.lines)):
                if self.lines[i].strip() == "---":
                    for raw in self.lines[1:i]:
                        m = re.match(r"^([A-Za-z_][\w-]*):\s*(.*?)\s*$", raw)
                        if m:
                            self.frontmatter[m.group(1)] = m.group(2).strip("\"'")
                    self.body_start = i + 1
                    return

    def _fence_mask(self) -> List[bool]:
        """Mark lines inside fenced code. A fence closes on the same character, a run
        at least as long as the opener, and no info string."""
        mask = [False] * len(self.lines)
        fence: Optional[str] = None
        for i in range(self.body_start, len(self.lines)):
            line = self.lines[i]
            if fence is None:
                m = FENCE_OPEN_RE.match(line)
                if m and not (m.group(1)[0] == "`" and "`" in m.group(2)):
                    fence = m.group(1)
                    mask[i] = True
            else:
                mask[i] = True
                m = FENCE_CLOSE_RE.match(line)
                if m and m.group(1)[0] == fence[0] and len(m.group(1)) >= len(fence):
                    fence = None
        return mask

    @property
    def title(self) -> str:
        if self.frontmatter.get("title"):
            return self.frontmatter["title"]
        for _, level, text in self.headings:
            if level == 1:
                return text
        return ""

    def prose_words(self) -> int:
        n = 0
        for i in range(self.body_start, len(self.lines)):
            if not self.in_fence[i]:
                n += len(self.lines[i].split())
        return n

    def tables(self) -> List[Tuple[int, List[List[str]]]]:
        """Return (first line no, rows) for each Markdown table outside fences.

        Outer pipes are optional and delimiter cells may use a single dash."""
        found: List[Tuple[int, List[List[str]]]] = []
        i = self.body_start
        n = len(self.lines)
        while i < n:
            line = self.lines[i]
            if (not self.in_fence[i] and "|" in line and i + 1 < n and not self.in_fence[i + 1]
                    and "|" in self.lines[i + 1] and DELIM_RE.match(self.lines[i + 1])
                    and len(split_row(line)) == len(split_row(self.lines[i + 1]))):
                start = i
                rows = [split_row(line)]
                i += 2
                while i < n and self.lines[i].strip() and "|" in self.lines[i] and not self.in_fence[i]:
                    rows.append(split_row(self.lines[i]))
                    i += 1
                found.append((start + 1, rows))
                continue
            i += 1
        return found


def split_row(line: str) -> List[str]:
    """Split a table row on pipes that are outside code spans and not escaped."""
    cells: List[str] = []
    buf: List[str] = []
    in_code = False
    s = line.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|") and not s.endswith("\\|"):
        s = s[:-1]
    j = 0
    while j < len(s):
        ch = s[j]
        if ch == "\\" and j + 1 < len(s):
            buf.append(s[j:j + 2])
            j += 2
            continue
        if ch == "`":
            in_code = not in_code
        if ch == "|" and not in_code:
            cells.append("".join(buf).strip())
            buf = []
        else:
            buf.append(ch)
        j += 1
    cells.append("".join(buf).strip())
    return cells


def cell_words(cell: str) -> int:
    return len(re.sub(r"`[^`]*`", "code", cell).split())


def check_page(page: Page) -> List[Finding]:
    p = str(page.path)
    out: List[Finding] = []

    # page-type-declared
    if page.type is None:
        out.append(Finding(p, 1, "page-type-declared", "no page type; add `type:` to frontmatter or pass --type"))
    elif page.type not in PAGE_TYPES:
        out.append(Finding(p, 1, "page-type-declared", "unknown page type '%s'; use one of %s" % (page.type, ", ".join(PAGE_TYPES))))

    if page.type in PAGE_TYPES:
        headings = [h[2] for h in page.headings]
        for desc, test in REQUIRED[page.type]:
            if not test(headings):
                out.append(Finding(p, 1, "required-sections", "%s page needs %s" % (page.type, desc)))
        body = "\n".join(l for i, l in enumerate(page.lines) if i >= page.body_start and not page.in_fence[i])
        if page.type == "concept" and not FIGURE_RE.search(body):
            out.append(Finding(p, 1, "required-sections", "concept page needs a figure"))
        if page.type == "how-to":
            steps = sum(1 for i, l in enumerate(page.lines) if i >= page.body_start and not page.in_fence[i] and ORDERED_RE.match(l))
            if steps < 2:
                out.append(Finding(p, 1, "required-sections", "how-to page needs numbered steps"))
        words = page.prose_words()
        budget = LENGTH_BUDGET[page.type]
        if words > budget:
            out.append(Finding(p, 1, "page-length-budget", "%d words; a %s page has a budget of %d" % (words, page.type, budget)))

    # heading-level-skip
    prev = 1 if page.frontmatter.get("title") else 0
    for line_no, level, text in page.headings:
        if prev and level > prev + 1:
            out.append(Finding(p, line_no, "heading-level-skip", "H%d follows H%d: '%s'" % (level, prev, text)))
        prev = level

    # tables
    for start, rows in page.tables():
        header = rows[0]
        if len(header) > MAX_TABLE_COLUMNS:
            out.append(Finding(p, start, "table-too-wide", "%d columns; the limit is %d" % (len(header), MAX_TABLE_COLUMNS)))
        body_rows = rows[1:]
        if len(body_rows) > MAX_TABLE_ROWS:
            out.append(Finding(p, start, "table-too-long", "%d rows; split it by theme (limit %d)" % (len(body_rows), MAX_TABLE_ROWS)))
        for offset, row in enumerate(rows):
            line_no = start + offset + (1 if offset else 0)
            for cell in row:
                if cell_words(cell) > MAX_CELL_WORDS:
                    out.append(Finding(p, line_no, "table-cell-too-long", "cell has %d words; the limit is %d" % (cell_words(cell), MAX_CELL_WORDS)))
                for code in re.findall(r"`([^`]+)`", cell):
                    if len(code) > MAX_CODE_CELL_CHARS:
                        out.append(Finding(p, line_no, "table-code-cell-long", "code span of %d characters in a cell" % len(code)))
        # table-without-intro: the previous non-blank line must be a sentence.
        k = start - 2
        while k >= page.body_start and not page.lines[k].strip():
            k -= 1
        prev_line = page.lines[k].strip() if k >= page.body_start else ""
        ok = bool(prev_line) and not prev_line.startswith(("#", "|", "```", "~~~", "<", ">", "![", "- ", "* ")) \
            and prev_line.endswith((".", ":", "?"))
        if not ok:
            out.append(Finding(p, start, "table-without-intro", "table is not introduced by a sentence"))
    return out


def topic_tokens(title: str) -> frozenset:
    words = re.findall(r"[a-z0-9]+", title.lower())
    return frozenset(w.rstrip("s") for w in words if w not in STOPWORDS)


def topic_headings(page: Page) -> set:
    """Level-2 headings that say something about the topic (not template headings)."""
    if page.type in ("reference", "status"):
        return set()
    return {h[2].lower() for h in page.headings if h[1] == 2 and not BOILERPLATE_HEADING_RE.fullmatch(h[2])}


def check_set(pages: List[Page]) -> List[Finding]:
    out: List[Finding] = []
    seen_titles: Dict[str, Page] = {}
    for page in pages:
        title = page.title
        if not title:
            continue
        key = " ".join(re.findall(r"[a-z0-9]+", title.lower()))
        if key in seen_titles:
            out.append(Finding(str(page.path), 1, "duplicate-title", "title '%s' also used by %s" % (title, seen_titles[key].path)))
        else:
            seen_titles[key] = page
    for i, a in enumerate(pages):
        for b in pages[:i]:
            if not a.title or not b.title or a.title.lower() == b.title.lower():
                continue
            ta, tb = topic_tokens(a.title), topic_tokens(b.title)
            union = ta | tb
            title_close = len(ta & tb) >= 2 and len(ta & tb) / len(union) >= 0.8
            ha, hb = topic_headings(a), topic_headings(b)
            shared = ha & hb
            heads_close = len(shared) >= 3 and len(shared) >= 0.6 * min(len(ha), len(hb))
            if title_close or heads_close:
                why = "similar title" if title_close else "%d shared section headings" % len(shared)
                out.append(Finding(str(a.path), 1, "near-duplicate-topic", "%s with %s; keep one page" % (why, b.path)))
    return out


def collect(paths: List[str]) -> List[Tuple[Path, bool]]:
    """Return (file, readme_ok). Only README.md named directly or at the top of a
    directory argument may be typed as a readme by its name."""
    files: List[Tuple[Path, bool]] = []
    for raw in paths:
        p = Path(raw)
        if p.is_dir():
            for x in sorted(p.rglob("*")):
                if x.suffix in (".md", ".mdx") and ".git" not in x.parts and "node_modules" not in x.parts:
                    files.append((x, x.parent == p))
        else:
            files.append((p, True))
    return files


def vale_sections(spec: str, catalog: Dict[str, Dict[str, object]]) -> str:
    """Print .vale.ini sections that apply each Vale rule's `applies_to` per path glob."""
    out: List[str] = []
    for item in spec.split(","):
        kind, _, glob = item.partition("=")
        kind = kind.strip()
        if kind not in PAGE_TYPE_SET or not glob.strip():
            raise SystemExit("--vale-sections needs TYPE=GLOB pairs; TYPE is one of %s" % ", ".join(PAGE_TYPES))
        off = sorted(r for r, d in catalog.items()
                     if d.get("check") == "vale" and "all" not in d["applies_to"] and kind not in d["applies_to"])  # type: ignore[operator]
        out.append("[%s]" % glob.strip())
        out.extend("AgentIX.%s = NO" % "".join(w.capitalize() for w in r.split("-")) for r in off)
        out.append("")
    return "\n".join(out)


def applies(rule: Dict[str, object], page_type: Optional[str]) -> bool:
    scope = rule.get("applies_to") or ["all"]
    return page_type is None or "all" in scope or page_type in scope  # type: ignore[operator]


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", help="Markdown files or directories")
    ap.add_argument("--check", action="store_true", help="exit 1 when any finding is reported")
    ap.add_argument("--type", choices=PAGE_TYPES, help="page type for files without a `type:` frontmatter key")
    ap.add_argument("--level", choices=LEVELS, default="warning", help="lowest severity to report (default: warning)")
    ap.add_argument("--ignore", default="", help="comma-separated rule ids to skip")
    ap.add_argument("--catalog", type=Path, default=CATALOG, help="rule catalog (default: the skill's catalog.yaml)")
    ap.add_argument("--vale-sections", metavar="TYPE=GLOB,...", help="print .vale.ini sections that apply applies_to per glob, then exit")
    args = ap.parse_args(argv)

    catalog = load_catalog(args.catalog)
    if args.vale_sections:
        print(vale_sections(args.vale_sections, catalog))
        return 0
    if not args.paths:
        ap.error("PATH is required")
    ignore = {x.strip() for x in args.ignore.split(",") if x.strip()}
    floor = LEVELS.index(args.level)

    pages: List[Page] = []
    unreadable = 0
    for path, readme_ok in collect(args.paths):
        try:
            pages.append(Page(path, args.type, readme_ok))
        except (OSError, UnicodeDecodeError) as err:
            print("%s:1: internal: cannot read file: %s" % (path, err.__class__.__name__))
            unreadable = 1
    by_path = {str(pg.path): pg for pg in pages}
    findings: List[Finding] = []
    for page in pages:
        findings.extend(check_page(page))
    findings.extend(check_set(pages))

    shown = 0
    for f in sorted(findings, key=lambda x: (x.path, x.line, x.rule)):
        if f.rule not in catalog:
            print("%s:%d: internal: rule id '%s' is not in the catalog" % (f.path, f.line, f.rule), file=sys.stderr)
            return 2
        rule = catalog[f.rule]
        if f.rule in ignore or LEVELS.index(str(rule["severity"])) < floor:
            continue
        if not applies(rule, by_path[f.path].type if f.path in by_path else None):
            continue
        print("%s:%d: %s: %s" % f)
        shown += 1
    if unreadable:
        return 2
    return 1 if (args.check and shown) else 0


if __name__ == "__main__":
    sys.exit(main())
