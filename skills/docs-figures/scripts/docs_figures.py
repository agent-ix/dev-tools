"""Figures for GitHub documentation: transparent light and dark SVGs.

Every label is drawn as glyph outlines, so GitHub's <img> (which cannot load
fonts) shows the same lettering everywhere. Figures may animate with CSS,
which plays inside <img>; an animated figure also gets a NAME-still pair, its
resting state. Readers who prefer reduced motion see the resting state.

A style (styles/NAME.json) supplies fonts, sizes, a palette per theme and,
optionally, a sketch renderer that draws every stroke by hand.
Call use_style() before building figures; render.py does this for you.
Requires fontTools. Fonts download once into $DOCS_FIGURES_CACHE
(default ~/.cache/docs-figures).
"""

import hashlib
import json
import math
import os
import random
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


class FontChain:
    """A font with fallbacks for characters it lacks, measured and drawn per glyph."""

    def __init__(self, key, fonts):
        self.key = key
        self.fonts = fonts
        self.upem = fonts[0].upem

    def pick(self, ch):
        for index, font in enumerate(self.fonts):
            if ord(ch) in font.cmap:
                return index, font
        raise ValueError(f"{self.key} has no glyph for {ch!r}")

    def width(self, text, size):
        if len(self.fonts) == 1:
            return self.fonts[0].width(text, size)
        total = 0
        for ch in text:
            _, font = self.pick(ch)
            total += font.advance(ch) * size / font.upem
        return total

    def runs(self, text):
        """Consecutive (font index, font, characters) runs."""
        out = []
        for ch in text:
            index, font = self.pick(ch)
            if out and out[-1][0] == index:
                out[-1][2].append(ch)
            else:
                out.append((index, font, [ch]))
        return [(index, font, "".join(chars)) for index, font, chars in out]


def use_style(name="ix-docs"):
    """Activate a style: load its palette and sizes and download its fonts."""
    global STYLE, THEMES, TITLE, LABEL, SMALL, MARGIN
    path = STYLES / f"{name}.json"
    if not path.exists():
        raise ValueError(f"unknown style {name!r}; available: {sorted(p.stem for p in STYLES.glob('*.json'))}")
    STYLE = json.loads(path.read_text(encoding="utf-8"))
    STYLE.setdefault("render", {"mode": "clean"})
    THEMES = STYLE["themes"]
    sizes = STYLE["sizes"]
    TITLE, LABEL, SMALL, MARGIN = sizes["title"], sizes["label"], sizes["small"], sizes["margin"]
    CACHE.mkdir(parents=True, exist_ok=True)
    FONTS.clear()
    for key, urls in STYLE["fonts"].items():
        faces = []
        for url in [urls] if isinstance(urls, str) else urls:
            font = CACHE / url.rsplit("/", 1)[1]
            if not font.exists():
                with urllib.request.urlopen(url, timeout=60) as response:
                    font.write_bytes(response.read())
            faces.append(Font(key, font))
        FONTS[key] = FontChain(key, faces)
    return STYLE


class Sketch:
    """Hand-drawn strokes. Jitter is seeded by the figure name, so a figure
    draws the same wobble in every theme and on every run."""

    def __init__(self, seed, settings):
        digest = hashlib.sha256(seed.encode("utf-8")).hexdigest()
        self.rng = random.Random(int(digest[:16], 16))
        self.rough = settings.get("roughness", 1.0)
        self.box_rough = settings.get("box_roughness", self.rough)
        self.overshoot = settings.get("box_overshoot", 2.5)
        self.passes = settings.get("passes", 2)
        self.pressure = settings.get("pressure", [0.9, 0.55])
        self.gap = settings.get("hatch_gap", 6)
        self.angle = math.radians(settings.get("hatch_angle", -41))

    def _j(self, amount):
        return self.rng.uniform(-amount, amount)

    def _curve(self, a, b, rough, overshoot=0.0, wander_ends=(True, True)):
        """Geometry of one pen stroke from a to b: endpoints wander, the middle bows."""
        (ax, ay), (bx, by) = a, b
        length = math.hypot(bx - ax, by - ay) or 1.0
        ux, uy = (bx - ax) / length, (by - ay) / length
        nx, ny = -uy, ux
        start = overshoot * self.rng.uniform(0.2, 1.0)
        end = overshoot * self.rng.uniform(0.2, 1.0)
        wander = rough * min(1.2, 0.3 + length / 100)
        o1 = self._j(wander) if wander_ends[0] else 0.0
        o2 = self._j(wander) if wander_ends[1] else 0.0
        p1 = (ax - ux * start + nx * o1, ay - uy * start + ny * o1)
        p2 = (bx + ux * end + nx * o2, by + uy * end + ny * o2)
        bow = self._j(rough * min(2.0, length / 50))
        c1 = (p1[0] + (p2[0] - p1[0]) * 0.3 + nx * bow * self.rng.uniform(0.7, 1.1),
              p1[1] + (p2[1] - p1[1]) * 0.3 + ny * bow * self.rng.uniform(0.7, 1.1))
        c2 = (p1[0] + (p2[0] - p1[0]) * 0.7 + nx * bow * self.rng.uniform(0.7, 1.1),
              p1[1] + (p2[1] - p1[1]) * 0.7 + ny * bow * self.rng.uniform(0.7, 1.1))
        return [p1, c1, c2, p2]

    def _retrace(self, points, amount, pinned=()):
        """A second pass that follows the first closely, as a hand re-tracing would."""
        return [p if i in pinned else (p[0] + self._j(amount), p[1] + self._j(amount)) for i, p in enumerate(points)]

    @staticmethod
    def _path(points):
        """Path data for a start point followed by cubic segments (3 points each)."""
        head, rest = points[0], points[1:]
        curves = " ".join(
            f"C{num(rest[i][0])},{num(rest[i][1])} {num(rest[i + 1][0])},{num(rest[i + 1][1])} "
            f"{num(rest[i + 2][0])},{num(rest[i + 2][1])}"
            for i in range(0, len(rest), 3)
        )
        return f"M{num(head[0])},{num(head[1])} {curves}"

    def segment(self, a, b, rough=None, overshoot=0.0):
        """One pen stroke from a to b, as path data."""
        return self._path(self._curve(a, b, self.rough if rough is None else rough, overshoot))

    def strokes(self, segments, overshoot=0.0, rough=None):
        """Each segment as a stroke plus close re-traces; returns (pass, path data) pairs."""
        rough = self.rough if rough is None else rough
        out = []
        for a, b in segments:
            first = self._curve(a, b, rough, overshoot)
            out.append((0, self._path(first)))
            for index in range(1, self.passes):
                out.append((index, self._path(self._retrace(first, 0.35 + 0.45 * rough))))
        return out

    def outline(self, x, y, w, h):
        corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
        return self.strokes(list(zip(corners, corners[1:] + corners[:1])), self.overshoot, self.box_rough)

    def polyline(self, points):
        """A bent edge drawn in one continuous stroke that ends exactly on its last point."""
        joints = [points[0]] + [(x + self._j(0.4), y + self._j(0.4)) for x, y in points[1:-1]] + [points[-1]]
        first = [joints[0]]
        for i, (a, b) in enumerate(zip(joints, joints[1:])):
            curve = self._curve(a, b, self.rough, wander_ends=(False, False))
            first += curve[1:]
        out = [(0, self._path(first))]
        last = len(first) - 1
        for index in range(1, self.passes):
            out.append((index, self._path(self._retrace(first, 0.3 + 0.35 * self.rough, pinned=(last,)))))
        return out

    def arrowhead(self, points, weight):
        """An open V at the last point, aligned with the final segment."""
        (px, py), (tx, ty) = points[-2], points[-1]
        length = math.hypot(tx - px, ty - py) or 1.0
        ux, uy = (tx - px) / length, (ty - py) / length
        size = 6.5 + 1.5 * weight
        wings = []
        for side in (1, -1):
            angle = math.radians(27 + self._j(4)) * side
            wx = tx - size * (ux * math.cos(angle) - uy * math.sin(angle))
            wy = ty - size * (uy * math.cos(angle) + ux * math.sin(angle))
            wings.append(((tx, ty), (wx, wy)))
        return [(i % self.passes, self.segment(a, b, 0.25)) for i, (a, b) in enumerate(wings * self.passes)]

    def hatch(self, x, y, w, h, gap=None):
        """Parallel pencil strokes clipped to the rectangle."""
        gap = gap or self.gap
        dx, dy = math.cos(self.angle), math.sin(self.angle)
        nx, ny = -dy, dx
        corners = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
        reach = [cx * nx + cy * ny for cx, cy in corners]
        lines = []
        offset = min(reach) + gap * self.rng.uniform(0.3, 0.7)
        while offset < max(reach):
            lo, hi = -1e9, 1e9
            px, py = offset * nx, offset * ny
            for p, d, low, high in ((px, dx, x + 1.5, x + w - 1.5), (py, dy, y + 1.5, y + h - 1.5)):
                if abs(d) < 1e-9:
                    if not low <= p <= high:
                        lo, hi = 1, 0
                    continue
                t1, t2 = (low - p) / d, (high - p) / d
                lo, hi = max(lo, min(t1, t2)), min(hi, max(t1, t2))
            if hi - lo > 2:
                a = (px + dx * lo, py + dy * lo)
                b = (px + dx * hi, py + dy * hi)
                lines.append((0, self.segment(a, b, self.rough * 0.4)))
            offset += gap * self.rng.uniform(0.85, 1.15)
        return lines

    def circle(self, cx, cy, r):
        out = []
        for index in range(self.passes):
            rx, ry = r * (1 + self._j(0.1)), r * (1 + self._j(0.1))
            start = self.rng.uniform(0, 2 * math.pi)
            x0, y0 = cx + rx * math.cos(start), cy + ry * math.sin(start)
            x1, y1 = cx - rx * math.cos(start), cy - ry * math.sin(start)
            rot = num(math.degrees(start))
            out.append((index, f"M{num(x0)},{num(y0)} A{num(rx)},{num(ry)} {rot} 1 1 {num(x1)},{num(y1)} "
                               f"A{num(rx)},{num(ry)} {rot} 1 1 {num(x0 + self._j(1))},{num(y0 + self._j(1))}"))
        return out


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

    def _sketched(self, sketch, item, pal, cls):
        """A rect, line, dot or bar drawn as pencil strokes in one group."""
        kind, paths = item[0], []

        def stroke(strokes, colour, width, dashed=False, opacity=None):
            dash = ' stroke-dasharray="5 4"' if dashed else ""
            for index, d in strokes:
                alpha = opacity if opacity is not None else sketch.pressure[min(index, len(sketch.pressure) - 1)]
                paths.append(f'<path d="{d}" stroke="{colour}" stroke-width="{num(width)}" stroke-opacity="{alpha}"{dash}/>')

        hatch_alpha = pal.get("hatch", pal["tint"])
        if kind == "rect":
            _, x, y, w, h, role, dashed, fill, has_stroke, weight, opacity, rx = item
            c = pal[role]
            if fill and has_stroke:
                stroke(sketch.hatch(x, y, w, h), c, 0.9, opacity=hatch_alpha)
            elif fill:
                solid = (opacity if opacity is not None else pal["tint"]) >= 0.9
                alpha = 1 if solid else min(1, (opacity if opacity is not None else pal["tint"]) * 2.2)
                stroke(sketch.hatch(x, y, w, h, gap=2.5 if solid else 7), c, 0.9, opacity=alpha)
            if has_stroke:
                stroke(sketch.outline(x, y, w, h), c, weight or (1.2 if role == "ink" else 1.5), dashed)
        elif kind == "line":
            _, points, role, weight, dashed, arrow = item
            stroke(sketch.polyline(points), pal[role], weight * 0.85, dashed)
            if arrow:
                stroke(sketch.arrowhead(points, weight), pal[role], weight * 0.85)
        elif kind == "bar":
            _, x, y, w, h, role = item
            stroke(sketch.hatch(x, y, w, h, gap=2.5), pal[role], 0.9, opacity=0.9)
            stroke(sketch.outline(x, y, w, h), pal[role], 1.2)
        else:
            _, cx, cy, r, role, filled = item
            for index, d in sketch.circle(cx, cy, r):
                fill = f' fill="{pal[role]}"' if filled and index == 0 else ""
                alpha = sketch.pressure[min(index, len(sketch.pressure) - 1)]
                paths.append(f'<path d="{d}"{fill} stroke="{pal[role]}" stroke-width="1.3" stroke-opacity="{alpha}"/>')
        tremor = ' filter="url(#tremor)"' if STYLE["render"].get("tremor") else ""
        return f'<g{cls}{tremor} fill="none" stroke-linecap="round">{"".join(paths)}</g>'

    # ---- rendering -------------------------------------------------------
    def render(self, theme, still=False):
        """SVG text; `still` drops the animation, leaving its resting state."""
        pal = THEMES[theme]
        used = {}
        body = []
        markers = set()
        sketch = Sketch(self.name, STYLE["render"]) if STYLE["render"].get("mode") == "sketch" else None
        for idx, item in enumerate(self.items):
            kind = item[0]
            cls = self._cls(idx)
            if sketch and kind in ("rect", "line", "dot", "bar"):
                body.append(self._sketched(sketch, item, pal, cls))
            elif kind == "rect":
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
                groups, cursor = [], x
                for index, face, chars in FONTS[font].runs(s):
                    k = size / face.upem
                    prefix = font if index == 0 else f"{font}{index}"
                    uses, adv = [], 0
                    for ch in chars:
                        name = face.glyph(ch)
                        if face.outline(name):
                            gid = re.sub(r"[^A-Za-z0-9]", "_", f"{prefix}-{name}")
                            used[gid] = face.outline(name)
                            uses.append(f'<use href="#{gid}" x="{adv}"/>')
                        adv += face.advance(ch)
                    groups.append((cursor, k, uses))
                    cursor += adv * k
                if len(groups) == 1:
                    gx, k, uses = groups[0]
                    body.append(
                        f'<g{cls} fill="{pal[role]}" transform="translate({num(gx)},{num(y)}) '
                        f'scale({k:.5f},{-k:.5f})">{"".join(uses)}</g>'
                    )
                else:
                    inner = "".join(
                        f'<g transform="translate({num(gx)},{num(y)}) scale({k:.5f},{-k:.5f})">{"".join(uses)}</g>'
                        for gx, k, uses in groups
                    )
                    body.append(f'<g{cls} fill="{pal[role]}">{inner}</g>')
        defs = [f'<path id="{gid}" d="{d}"/>' for gid, d in sorted(used.items())]
        for role in sorted(markers):
            defs.append(
                f'<marker id="ah-{role}" viewBox="0 0 10 10" refX="9" refY="5" '
                f'markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
                f'<path d="M0,0 L10,5 L0,10 z" fill="{pal[role]}"/></marker>'
            )
        tremor = STYLE["render"].get("tremor") if sketch else None
        if tremor:
            defs.append(
                f'<filter id="tremor" x="-5%" y="-5%" width="110%" height="110%">'
                f'<feTurbulence type="fractalNoise" baseFrequency="{tremor["frequency"]}" numOctaves="2" seed="7"/>'
                f'<feDisplacementMap in="SourceGraphic" scale="{tremor["scale"]}" xChannelSelector="R" yChannelSelector="G"/>'
                f"</filter>"
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
