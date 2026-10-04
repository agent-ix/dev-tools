"""Figures for GitHub documentation: transparent light and dark SVGs.

Every label is drawn as glyph outlines, so GitHub's <img> (which cannot load
fonts) shows the same lettering everywhere. Figures may animate with CSS,
which plays inside <img>; an animated figure also gets a NAME-still pair, its
resting state. Readers who prefer reduced motion see the resting state.

A style (styles/NAME.json) supplies fonts, sizes and a palette per theme.
Call use_style() before building figures; render.py does this for you.
Requires fontTools. Fonts download once into $DOCS_FIGURES_CACHE
(default ~/.cache/docs-figures).
"""

import json
import os
import re
import urllib.request
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont

STYLES = Path(__file__).resolve().parents[1] / "styles"
CACHE = Path(os.environ.get("DOCS_FIGURES_CACHE", Path.home() / ".cache" / "docs-figures"))

# Set by use_style(); figure modules import these after a style is active.
STYLE = None
THEMES = {}
FONTS = {}
TITLE = LABEL = SMALL = MARGIN = None
FADE = 0.2  # seconds an animated element takes to light up or fade


class Font:
    """Glyph advances and outlines for one font face."""

    def __init__(self, key, path):
        self.key = key
        tt = TTFont(path)
        self.upem = tt["head"].unitsPerEm
        self.cmap = tt.getBestCmap()
        self.glyphs = tt.getGlyphSet()
        self.hmtx = tt["hmtx"]
        self._outlines = {}

    def glyph(self, ch):
        name = self.cmap.get(ord(ch))
        if name is None:
            raise ValueError(f"{self.key} has no glyph for {ch!r}")
        return name

    def advance(self, ch):
        return self.hmtx[self.glyph(ch)][0]

    def width(self, text, size):
        return sum(self.advance(ch) for ch in text) * size / self.upem

    def outline(self, name):
        if name not in self._outlines:
            pen = SVGPathPen(self.glyphs, ntos=lambda v: str(round(v)))
            self.glyphs[name].draw(pen)
            self._outlines[name] = pen.getCommands()
        return self._outlines[name]


def use_style(name="ix-docs"):
    """Activate a style: load its palette and sizes and download its fonts."""
    global STYLE, THEMES, TITLE, LABEL, SMALL, MARGIN
    path = STYLES / f"{name}.json"
    if not path.exists():
        raise ValueError(f"unknown style {name!r}; available: {sorted(p.stem for p in STYLES.glob('*.json'))}")
    STYLE = json.loads(path.read_text(encoding="utf-8"))
    THEMES = STYLE["themes"]
    sizes = STYLE["sizes"]
    TITLE, LABEL, SMALL, MARGIN = sizes["title"], sizes["label"], sizes["small"], sizes["margin"]
    CACHE.mkdir(parents=True, exist_ok=True)
    FONTS.clear()
    for key, url in STYLE["fonts"].items():
        font = CACHE / url.rsplit("/", 1)[1]
        if not font.exists():
            with urllib.request.urlopen(url, timeout=60) as response:
                font.write_bytes(response.read())
        FONTS[key] = Font(key, font)
    return STYLE


def num(v):
    return f"{v:.1f}".rstrip("0").rstrip(".")


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


class Figure:
    """A fixed-size drawing whose colours resolve per theme at render time.

    With a `cycle` (seconds), elements can carry animation windows: "lit"
    elements are dimmed outside their windows, "appear" elements are hidden
    outside theirs, and tokens travel along an edge. Without animation every
    element shows its static state, which is also what readers who prefer
    reduced motion see.
    """

    def __init__(self, name, width, height, label, cycle=None, still_label=None):
        self.name = name
        self.width = width
        self.height = height
        self.label = label
        self.still_label = still_label or label
        self.cycle = cycle
        self.items = []
        self.anim = {}

    def _add(self, item, anim=None):
        self.items.append(item)
        if anim:
            if self.cycle is None:
                raise ValueError(f"{self.name}: animation needs a cycle")
            self.anim[len(self.items) - 1] = anim

    # ---- primitives -------------------------------------------------------
    def text(self, x, y, s, role="ink", font="mono", size=None, anchor="start", anim=None):
        size = LABEL if size is None else size
        w = FONTS[font].width(s, size)
        x0 = {"start": x, "middle": x - w / 2, "end": x - w}[anchor]
        if x0 < -MARGIN or x0 + w > self.width + MARGIN or y - size < -MARGIN or y > self.height + MARGIN:
            raise ValueError(f"{self.name}: text {s!r} leaves the canvas")
        self._add(("text", x0, y, s, role, font, size), anim)
        return w

    def rect(self, x, y, w, h, role, dashed=False, fill=True, stroke=True, weight=None, opacity=None, rx=4, anim=None):
        self._add(("rect", x, y, w, h, role, dashed, fill, stroke, weight, opacity, rx), anim)

    def line(self, points, role="ink", weight=1.5, dashed=False, arrow=True, anim=None):
        self._add(("line", points, role, weight, dashed, arrow), anim)

    def dot(self, cx, cy, r, role, filled=True):
        self._add(("dot", cx, cy, r, role, filled))

    def bar(self, x, y, w, h, role):
        """A horizontal bar anchored at x with a rounded data end."""
        if w <= 0:
            return
        self._add(("bar", x, y, w, h, role))

    def token(self, points, role, start, end):
        """A dot that travels along `points` between `start` and `end` seconds."""
        self._add(("token", points, role), ("token", points, start, end))

    # ---- composites ------------------------------------------------------
    def box(self, x, y, w, h, role, title, *subs, dashed=False, sub_role="muted", lit=None):
        """A node: tinted box, title in the role colour, muted sub-lines.

        A title or sub-line may be a list of (text, windows, static) variants
        that appear only during their windows; `lit` dims the rest outside
        its windows.
        """
        glow = ("lit", lit) if lit else None
        self.rect(x, y, w, h, role, dashed=dashed, fill=role != "ink", anim=glow)
        lines = [(title, "title", TITLE, role)] + [(s, "mono", SMALL, sub_role) for s in subs]
        heights = [19 if f == "title" else 17 for _, f, _, _ in lines]
        if sum(heights) + 8 > h:
            raise ValueError(f"{self.name}: box {lines[0][0]!r} is too short")
        top = y + (h - sum(heights)) / 2
        for (s, f, size, r), lh in zip(lines, heights):
            variants = s if isinstance(s, list) else [(s, None, True)]
            for text, windows, static in variants:
                need = FONTS[f].width(text, size) + 16
                if need > w:
                    raise ValueError(f"{self.name}: {text!r} needs {need:.0f}px, box is {w}px")
                anim = ("appear", windows, static) if windows else glow
                self.text(x + w / 2, top + lh * 0.74, text, r, f, size, "middle", anim)
            top += lh

    def note(self, x, y, s, role, sub=None, anchor="start", anim=None):
        self.text(x, y, s, role, "mono", LABEL, anchor, anim)
        if sub:
            self.text(x, y + 16, sub, "muted", "mono", SMALL, anchor, anim)

    def legend(self, x, y, entries):
        for role, s, kind in entries:
            if kind == "box":
                self.rect(x, y - 10, 11, 11, role, weight=1.2, rx=2)
            elif kind == "dash":
                self.line([(x, y - 4), (x + 22, y - 4)], role, 1.5, dashed=True, arrow=False)
                x += 11
            else:
                self.rect(x, y - 10, 11, 11, role, stroke=False, opacity=1, rx=2)
            w = self.text(x + 17, y, s, "muted", "mono", SMALL)
            x += 17 + w + 24

    # ---- animation -------------------------------------------------------
    def _pct(self, t):
        return f"{100 * min(max(t, 0), self.cycle) / self.cycle:.3f}".rstrip("0").rstrip(".")

    def _keyframes(self, idx, spec):
        if spec[0] == "token":
            _, points, start, end = spec
            lengths = [abs(bx - ax) + abs(by - ay) for (ax, ay), (bx, by) in zip(points, points[1:])]
            total = sum(lengths)
            frames = [(0, points[0], 0), (start - 0.01, points[0], 0), (start, points[0], 1)]
            done = 0
            for length, point in zip(lengths, points[1:]):
                done += length
                frames.append((start + (end - start) * done / total, point, 1))
            frames += [(end + 0.15, points[-1], 0), (self.cycle, points[-1], 0)]
            body = " ".join(
                f"{self._pct(t)}%{{transform:translate({num(px)}px,{num(py)}px);opacity:{o}}}"
                for t, (px, py), o in frames
            )
        else:
            mode, windows = spec[0], spec[1]
            off = 0.22 if mode == "lit" else 0
            frames = [(0, off)]
            for start, end in windows:
                frames += [(start - FADE, off), (start, 1), (end, 1), (end + FADE, off)]
            frames.append((self.cycle, off))
            body = " ".join(f"{self._pct(t)}%{{opacity:{o}}}" for t, o in frames)
        return f"@keyframes k{idx}{{{body}}}.a{idx}{{animation:k{idx} {num(self.cycle)}s linear infinite}}"

    def _cls(self, idx):
        spec = self.anim.get(idx)
        if spec is None:
            return ""
        hidden = spec[0] == "token" or (spec[0] == "appear" and not spec[2])
        return f' class="a{idx}"' + (' opacity="0"' if hidden else "")

    # ---- rendering -------------------------------------------------------
    def render(self, theme, still=False):
        """SVG text; `still` drops the animation, leaving its resting state."""
        pal = THEMES[theme]
        used = {}
        body = []
        markers = set()
        for idx, item in enumerate(self.items):
            kind = item[0]
            cls = self._cls(idx)
            if kind == "rect":
                _, x, y, w, h, role, dashed, fill, stroke, weight, opacity, rx = item
                c = pal[role]
                attrs = [f'x="{num(x)}" y="{num(y)}" width="{num(w)}" height="{num(h)}" rx="{rx}"']
                if stroke:
                    attrs.append(f'stroke="{c}" stroke-width="{weight or (1.2 if role == "ink" else 1.5)}"')
                if fill:
                    attrs.append(f'fill="{c}" fill-opacity="{opacity if opacity is not None else pal["tint"]}"')
                else:
                    attrs.append('fill="none"')
                if dashed:
                    attrs.append('stroke-dasharray="5 4"')
                body.append(f"<rect{cls} {' '.join(attrs)}/>")
            elif kind == "line":
                _, points, role, weight, dashed, arrow = item
                d = "M" + " L".join(f"{num(px)},{num(py)}" for px, py in points)
                attrs = [f'd="{d}" fill="none" stroke="{pal[role]}" stroke-width="{weight}"']
                if dashed:
                    attrs.append('stroke-dasharray="5 4"')
                if arrow:
                    markers.add(role)
                    attrs.append(f'marker-end="url(#ah-{role})"')
                body.append(f"<path{cls} {' '.join(attrs)}/>")
            elif kind == "token":
                _, points, role = item
                body.append(f'<circle{cls} r="5" fill="{pal[role]}" stroke="{pal[role]}" stroke-opacity="0.35" stroke-width="5"/>')
            elif kind == "dot":
                _, cx, cy, r, role, filled = item
                c = pal[role]
                fill = f'fill="{c}"' if filled else 'fill="none"'
                body.append(f'<circle cx="{num(cx)}" cy="{num(cy)}" r="{r}" {fill} stroke="{c}" stroke-width="1.5"/>')
            elif kind == "bar":
                _, x, y, w, h, role = item
                r = min(4, h / 2, w)
                d = (
                    f"M{num(x)},{num(y)} H{num(x + w - r)} A{num(r)},{num(r)} 0 0 1 {num(x + w)},{num(y + r)} "
                    f"V{num(y + h - r)} A{num(r)},{num(r)} 0 0 1 {num(x + w - r)},{num(y + h)} H{num(x)} Z"
                )
                body.append(f'<path d="{d}" fill="{pal[role]}"/>')
            else:
                _, x, y, s, role, font, size = item
                f = FONTS[font]
                k = size / f.upem
                uses = []
                adv = 0
                for ch in s:
                    name = f.glyph(ch)
                    if f.outline(name):
                        gid = re.sub(r"[^A-Za-z0-9]", "_", f"{font}-{name}")
                        used[gid] = f.outline(name)
                        uses.append(f'<use href="#{gid}" x="{adv}"/>')
                    adv += f.advance(ch)
                body.append(
                    f'<g{cls} fill="{pal[role]}" transform="translate({num(x)},{num(y)}) '
                    f'scale({k:.5f},{-k:.5f})">{"".join(uses)}</g>'
                )
        defs = [f'<path id="{gid}" d="{d}"/>' for gid, d in sorted(used.items())]
        for role in sorted(markers):
            defs.append(
                f'<marker id="ah-{role}" viewBox="0 0 10 10" refX="9" refY="5" '
                f'markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
                f'<path d="M0,0 L10,5 L0,10 z" fill="{pal[role]}"/></marker>'
            )
        style = ""
        if self.anim and not still:
            rules = "".join(self._keyframes(idx, spec) for idx, spec in sorted(self.anim.items()))
            style = (
                f"<style>{rules}"
                "@media (prefers-reduced-motion: reduce){*{animation:none!important}}</style>\n"
            )
        label = esc(self.still_label if still else self.label)
        w, h = self.width + 2 * MARGIN, self.height + 2 * MARGIN
        return (
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
            f'viewBox="{-MARGIN} {-MARGIN} {w} {h}" role="img" aria-label="{label}">\n'
            f"<title>{label}</title>\n{style}<defs>{''.join(defs)}</defs>\n" + "\n".join(body) + "\n</svg>\n"
        )

    def outputs(self):
        """File name to SVG text for every theme, plus still versions when animated."""
        files = {}
        for theme in THEMES:
            files[f"{self.name}-{theme}.svg"] = self.render(theme)
            if self.anim:
                files[f"{self.name}-still-{theme}.svg"] = self.render(theme, still=True)
        return files

    def save(self, out):
        """Write every output into the directory `out`; returns the paths."""
        paths = []
        for name, svg in self.outputs().items():
            path = Path(out) / name
            path.write_text(svg, encoding="utf-8")
            paths.append(path)
        return paths


def wrap(text, width, size=None):
    """Split text into lines that fit `width` pixels in the mono face."""
    size = SMALL if size is None else size
    lines, line = [], ""
    for word in text.split():
        trial = f"{line} {word}".strip()
        if line and FONTS["mono"].width(trial, size) > width:
            lines.append(line)
            line = word
        else:
            line = trial
    return lines + [line]
