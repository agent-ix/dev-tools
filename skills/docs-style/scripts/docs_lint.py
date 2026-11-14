#!/usr/bin/env python3
"""Structure checks for Markdown docs that Vale cannot express.

Usage:
    docs_lint.py [--check] [--type TYPE] [--level LEVEL] PATH...

PATH is a Markdown file or a directory (searched for .md and .mdx files).
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

FIGURE_RE = re.compile(r"!\[|<img\b|<picture\b|<figure\b|<svg\b", re.I)
HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
FENCE_RE = re.compile(r"^\s*(```|~~~)")
ORDERED_RE = re.compile(r"^\s*\d+[.)]\s+\S")
STOPWORDS = {"the", "a", "an", "of", "and", "how", "to", "in", "for", "is", "what", "with", "on"}

# Required sections per page type: (description, test over the list of headings).
REQUIRED = {
    "concept": [
        ("an 'Example' section", lambda hs: any(re.search(r"\bexample\b", h, re.I) for h in hs)),
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
        ("a Syntax, Usage, Options or Fields section", lambda hs: any(re.fullmatch(r"(syntax|usage|options|fields|commands)", h, re.I) for h in hs)),
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

    def __init__(self, path: Path, forced_type: Optional[str]) -> None:
        self.path = path
        self.lines = path.read_text(encoding="utf-8").splitlines()
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
        self.type = forced_type or self.frontmatter.get("type")
        if not self.type and path.name.lower() in ("readme.md", "readme.mdx"):
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
        mask = [False] * len(self.lines)
        fence: Optional[str] = None
        for i in range(self.body_start, len(self.lines)):
            m = FENCE_RE.match(self.lines[i])
            if fence is None and m:
                fence = m.group(1)
                mask[i] = True
            elif fence is not None:
                mask[i] = True
                if m and m.group(1) == fence:
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
        """Return (first line no, rows) for each Markdown table outside fences."""
        found: List[Tuple[int, List[List[str]]]] = []
        i = self.body_start
        while i < len(self.lines):
            line = self.lines[i]
            if (not self.in_fence[i] and line.lstrip().startswith("|") and i + 1 < len(self.lines)
                    and re.match(r"^\s*\|?[\s:|-]*-{3,}[\s:|-]*\|?\s*$", self.lines[i + 1])):
                start = i
                rows = [split_row(line)]
                i += 2
                while i < len(self.lines) and self.lines[i].lstrip().startswith("|") and not self.in_fence[i]:
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
    tail = "".join(buf).strip()
    if tail:
        cells.append(tail)
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
            title_close = bool(ta) and bool(tb) and (ta <= tb or tb <= ta) and len(ta & tb) >= 2
            ha = {h[2].lower() for h in a.headings if h[1] == 2}
            hb = {h[2].lower() for h in b.headings if h[1] == 2}
            shared = ha & hb
            heads_close = len(shared) >= 3 and len(shared) >= 0.6 * min(len(ha), len(hb))
            if title_close or heads_close:
                why = "similar title" if title_close else "%d shared section headings" % len(shared)
                out.append(Finding(str(a.path), 1, "near-duplicate-topic", "%s with %s; keep one page" % (why, b.path)))
    return out


def collect(paths: List[str]) -> List[Path]:
    files: List[Path] = []
    for raw in paths:
        p = Path(raw)
        if p.is_dir():
            files.extend(sorted(x for x in p.rglob("*") if x.suffix in (".md", ".mdx") and ".git" not in x.parts and "node_modules" not in x.parts))
        else:
            files.append(p)
    return files


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="+", help="Markdown files or directories")
    ap.add_argument("--check", action="store_true", help="exit 1 when any finding is reported")
    ap.add_argument("--type", choices=PAGE_TYPES, help="page type for files without a `type:` frontmatter key")
    ap.add_argument("--level", choices=LEVELS, default="warning", help="lowest severity to report (default: warning)")
    args = ap.parse_args(argv)

    catalog = load_catalog()
    floor = LEVELS.index(args.level)
    pages = [Page(f, args.type) for f in collect(args.paths)]
    findings: List[Finding] = []
    for page in pages:
        findings.extend(check_page(page))
    findings.extend(check_set(pages))

    shown = 0
    for f in sorted(findings, key=lambda x: (x.path, x.line, x.rule)):
        if f.rule not in catalog:
            print("%s:%d: internal: rule id '%s' is not in the catalog" % (f.path, f.line, f.rule), file=sys.stderr)
            return 2
        if LEVELS.index(str(catalog[f.rule]["severity"])) >= floor:
            print("%s:%d: %s: %s" % f)
            shown += 1
    return 1 if (args.check and shown) else 0


if __name__ == "__main__":
    sys.exit(main())
