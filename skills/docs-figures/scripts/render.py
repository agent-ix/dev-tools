#!/usr/bin/env python3
"""Render a repository's figure definitions to light and dark SVGs.

    python3 render.py docs/images/figures.py            # write SVGs next to it
    python3 render.py docs/images/figures.py --check    # exit 1 if any SVG is stale
    python3 render.py docs/images/figures.py --only how-it-works

The definitions file imports the engine as `docs_figures` and lists its figure
builders in FIGURES: functions that take no arguments and return a Figure.
It may set STYLE to a style name; --style overrides it.
"""

from __future__ import annotations

import argparse
import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True  # keep __pycache__ out of the skill and the repo

import docs_figures  # noqa: E402


def style_of(path: Path) -> str | None:
    """The STYLE assignment in a definitions file, read without importing it."""
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("STYLE = "):
            return line.split("=", 1)[1].strip().strip("\"'")
    return None


def load(path: Path):
    spec = importlib.util.spec_from_file_location(f"figures_{path.stem}", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if not getattr(module, "FIGURES", None):
        raise SystemExit(f"{path} defines no FIGURES list")
    return module


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("definitions", type=Path, help="Python file with a FIGURES list")
    parser.add_argument("--out", type=Path, help="output directory (default: the definitions file's directory)")
    parser.add_argument("--style", help="style name from styles/ (default: the file's STYLE, else ix-docs)")
    parser.add_argument("--only", action="append", default=[], help="render only figures with this name")
    parser.add_argument("--check", action="store_true", help="compare with the SVGs on disk instead of writing")
    args = parser.parse_args(argv)

    definitions = args.definitions.resolve()
    out = (args.out or definitions.parent).resolve()
    docs_figures.use_style(args.style or style_of(definitions) or "ix-docs")
    module = load(definitions)

    stale, written = [], 0
    for build in module.FIGURES:
        figure = build()
        if args.only and figure.name not in args.only:
            continue
        for name, svg in figure.outputs().items():
            path = out / name
            if args.check:
                if not path.exists() or path.read_text(encoding="utf-8") != svg:
                    stale.append(name)
            else:
                path.write_text(svg, encoding="utf-8")
                written += 1
    if args.check:
        for name in stale:
            print(f"stale: {name}", file=sys.stderr)
        return 1 if stale else 0
    print(f"wrote {written} SVGs to {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
