#!/usr/bin/env python3
"""Screenshot figures the way GitHub shows them, for visual review.

    python3 preview.py docs/images --out /tmp/figures
    python3 preview.py docs/images/flow-dark.svg --out /tmp/figures --at 1.5 4 8

For every NAME-light.svg / NAME-dark.svg pair it writes NAME.png: the light
SVG on #ffffff above the dark SVG on #0d1117, embedded as <img> at README
width. With --at, an animated figure also gets NAME-THEME@SECONDS.png frames.
Headless Chrome cannot advance time inside an <img>, so frames inline the SVG
and seek every animation to the given time. Needs Chrome or Chromium; set
CHROME to its path if it is not found.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

GROUND = {"light": "#ffffff", "dark": "#0d1117"}
MAC_CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"


def chrome() -> str:
    for candidate in (os.environ.get("CHROME"), MAC_CHROME):
        if candidate and Path(candidate).exists():
            return candidate
    for name in ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome"):
        found = shutil.which(name)
        if found:
            return found
    raise SystemExit("Chrome or Chromium not found; set CHROME to its path")


def size(svg: Path) -> tuple[float, float]:
    head = svg.read_text(encoding="utf-8")[:400]
    width = float(re.search(r'width="([\d.]+)"', head).group(1))
    height = float(re.search(r'height="([\d.]+)"', head).group(1))
    return width, height


def shoot(html: str, png: Path, width: int, height: int, work: Path) -> None:
    page = work / f"{png.stem}.html"
    page.write_text(html, encoding="utf-8")
    subprocess.run(
        [chrome(), "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1.5",
         f"--window-size={width},{height}", f"--screenshot={png}", page.as_uri()],
        check=True, capture_output=True, timeout=60,
    )


def pairs(paths: list[Path]) -> list[tuple[str, Path, Path]]:
    files = []
    for path in paths:
        files += sorted(path.glob("*-light.svg")) if path.is_dir() else [path]
    found = {}
    for svg in files:
        name = re.sub(r"-(light|dark)\.svg$", "", svg.name)
        light, dark = svg.with_name(f"{name}-light.svg"), svg.with_name(f"{name}-dark.svg")
        if light.exists() and dark.exists():
            found[name] = (name, light, dark)
    return list(found.values())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+", type=Path, help="SVG files or directories of NAME-light/dark.svg pairs")
    parser.add_argument("--out", type=Path, required=True, help="directory for the PNGs")
    parser.add_argument("--width", type=int, default=880, help="column width in CSS pixels (GitHub README is about 880)")
    parser.add_argument("--at", type=float, nargs="*", default=[], help="seconds at which to grab animation frames")
    args = parser.parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)

    shots = 0
    with tempfile.TemporaryDirectory(prefix="docs-figures-") as temp:
        work = Path(temp)
        for name, light, dark in pairs(args.paths):
            w, h = size(light)
            scaled = h * min(1, args.width / w)
            panels = "".join(
                f'<div style="background:{GROUND[t]};padding:20px"><img src="{svg.as_uri()}" '
                f'style="display:block;max-width:100%;width:{w}px"></div>'
                for t, svg in (("light", light), ("dark", dark))
            )
            html = f'<!doctype html><meta charset="utf-8"><body style="margin:0;width:{args.width + 40}px">{panels}'
            shoot(html, args.out / f"{name}.png", args.width + 40, int(2 * scaled + 80), work)
            shots += 1
            animated = "<style>" in light.read_text(encoding="utf-8")
            for t in args.at if animated else []:
                for theme, svg in (("light", light), ("dark", dark)):
                    html = (
                        f'<!doctype html><meta charset="utf-8"><body style="margin:0;background:{GROUND[theme]}">'
                        f'<div style="width:{args.width}px;padding:20px">{svg.read_text(encoding="utf-8")}</div>'
                        f"<script>document.querySelector('svg').style.cssText='width:100%;height:auto';"
                        f"document.getAnimations().forEach(a=>{{a.pause();a.currentTime={t * 1000}}});</script>"
                    )
                    shoot(html, args.out / f"{name}-{theme}@{t:g}.png", args.width + 40, int(scaled + 40), work)
                    shots += 1
    print(f"wrote {shots} PNGs to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
