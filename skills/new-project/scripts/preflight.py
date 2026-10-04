#!/usr/bin/env python3
"""Fail closed before a newly scaffolded project is committed or pushed."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

AGPL_SPDX = "AGPL-3.0-or-later"
AGPL_SHA256 = "0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0"
PROTECTED_SUFFIXES = {".doc", ".docx", ".pdf", ".xls", ".xlsx"}
TEXT_SUFFIXES = {
    "",
    ".css",
    ".html",
    ".js",
    ".json",
    ".jsx",
    ".md",
    ".mjs",
    ".py",
    ".rs",
    ".sh",
    ".toml",
    ".ts",
    ".tsx",
    ".txt",
    ".yaml",
    ".yml",
}
IGNORED_PARTS = {
    ".git",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
    "node_modules",
    "target",
}
BACKUP_MARKER = "dev" + "_bak"
RIGHTS_MARKER = "pending" + "-counsel"
# Split so this file never contains the literals it searches for, the same
# convention the two markers above use.
HOME_DIR = "/" + "home"
USERS_DIR = "/" + "Users"
ROOT_DIR = "/" + "root" + "/"
# A match preceded by a hostname character belongs to a URL, not to a
# workstation path: a devpi registry URL ending in the root user's index is
# the registry, not a home directory. A file: URL is preceded by a slash,
# which this still admits, so genuine workstation paths are unaffected.
NOT_IN_URL = r"(?<![\w.:])"
FORBIDDEN_TEXT = {
    "internal backup path": re.compile(
        rf"(?:^|[/\\]){re.escape(BACKUP_MARKER)}(?:[/\\]|$)"
    ),
    "unresolved rights marker": re.compile(re.escape(RIGHTS_MARKER), re.I),
    "absolute workstation path": re.compile(
        rf"{NOT_IN_URL}(?:(?:{HOME_DIR}|{USERS_DIR})/[^/\s]+/|{ROOT_DIR}|"
        r"[A-Za-z]:[\\/](?:Users|Documents and Settings)[\\/]"
        r"[^\\/\s]+[\\/])"
    ),
}


def _files(root: Path):
    for path in root.rglob("*"):
        if path.is_file() and not any(
            part in IGNORED_PARTS for part in path.relative_to(root).parts
        ):
            yield path


def _manifest_failures(root: Path) -> list[str]:
    failures: list[str] = []
    package = root / "package.json"
    if package.exists():
        value = json.loads(package.read_text())
        if value.get("license") != AGPL_SPDX:
            failures.append("package.json: license must be AGPL-3.0-or-later")

    for name in ("Cargo.toml", "pyproject.toml"):
        path = root / name
        if not path.exists():
            continue
        sections = {"package"} if name == "Cargo.toml" else {"project", "tool.poetry"}
        declared = _toml_licenses(path.read_text(), sections)
        if not declared or any(value != AGPL_SPDX for value in declared):
            failures.append(f"{name}: license must be AGPL-3.0-or-later")
    return failures


def _toml_licenses(value: str, sections: set[str]) -> list[str]:
    """Read only the license scalar needed by this dependency-free preflight."""
    section = ""
    declarations: list[str] = []
    for raw_line in value.splitlines():
        line = raw_line.split("#", 1)[0].strip()
        match = re.fullmatch(r"\[([^]]+)\]", line)
        if match:
            section = match.group(1).strip()
            continue
        if section not in sections or not line.startswith("license"):
            continue
        scalar = re.fullmatch(r"license\s*=\s*['\"]([^'\"]+)['\"]", line)
        if scalar:
            declarations.append(scalar.group(1))
            continue
        text_value = re.search(r"\btext\s*=\s*['\"]([^'\"]+)['\"]", line)
        if text_value:
            declarations.append(text_value.group(1))
            continue
        # A file-form declaration delegates to the root LICENSE, checked separately.
        if re.search(r"\bfile\s*=", line):
            declarations.append(AGPL_SPDX)
            continue
        declarations.append("<invalid-license-declaration>")
    return declarations


def check(root: Path) -> list[str]:
    failures: list[str] = []
    license_path = root / "LICENSE"
    if not license_path.exists():
        failures.append("LICENSE: canonical GNU AGPL version 3 text is required")
    else:
        license_digest = hashlib.sha256(license_path.read_bytes()).hexdigest()
        if license_digest != AGPL_SHA256:
            failures.append("LICENSE: canonical GNU AGPL version 3 text is required")

    rights = root / "CONTENT_RIGHTS.md"
    if not rights.exists():
        failures.append("CONTENT_RIGHTS.md: repository rights policy is required")
    elif "REPLACE_WITH_OWNER" in rights.read_text(errors="replace"):
        failures.append("CONTENT_RIGHTS.md: replace the project-owner placeholder")

    failures.extend(_manifest_failures(root))
    for path in _files(root):
        relative = path.relative_to(root).as_posix()
        if path.suffix.lower() in PROTECTED_SUFFIXES:
            failures.append(f"{relative}: protected source file type is not allowed")
            continue
        if path.suffix.lower() not in TEXT_SUFFIXES:
            failures.append(f"{relative}: unreviewed file type is not allowed")
            continue
        try:
            value = path.read_text()
        except UnicodeDecodeError:
            failures.append(f"{relative}: declared text file is not UTF-8")
            continue
        for label, pattern in FORBIDDEN_TEXT.items():
            if pattern.search(value):
                failures.append(f"{relative}: {label}")
    return sorted(set(failures))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("project", type=Path)
    args = parser.parse_args()
    root = args.project.resolve()
    if not root.is_dir():
        parser.error("project must be an existing directory")
    failures = check(root)
    if failures:
        print("new-project preflight failed:")
        for failure in failures:
            print(f"- {failure}")
        return 1
    print("new-project preflight passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
