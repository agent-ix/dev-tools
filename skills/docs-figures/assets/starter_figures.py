"""Documentation figures for this repository, rendered by the docs-figures skill.

Regenerate from the repository root with the agent-ix/dev-tools skill:

    python3 -m pip install fonttools
    python3 <dev-tools>/skills/docs-figures/scripts/render.py docs/images/figures.py

Add --check to confirm the committed SVGs are current. Replace the example
values below with values read from this repository's own data.
"""

from docs_figures import SMALL, TITLE, Figure

STYLE = "ix-docs"

ROLES = [
    ("code", "your code", "box"),
    ("model", "model answer", "box"),
    ("logic", "your rule", "box"),
    ("ink", "input / decision", "box"),
]


def flow():
    """A mechanism: what moves along each edge, labelled on the edge."""
    f = Figure("flow", 720, 112, "A request goes to the model, which answers 0.82; the rule compares it with 0.7 and decides true.")
    cy = 50
    f.box(0, cy - 28, 120, 56, "ink", "request", "the input")
    f.box(170, cy - 28, 150, 56, "model", "ask model", "1 question")
    f.box(370, cy - 28, 130, 56, "logic", "≥ 0.7", "true")
    f.box(550, cy - 28, 170, 56, "ink", "decision", "true + trace")
    f.line([(120, cy), (167, cy)], "ink")
    f.line([(320, cy), (367, cy)], "model")
    f.note(328, cy - 9, "0.82", "model")
    f.line([(500, cy), (547, cy)], "logic", weight=2.4)
    f.legend(0, 108, ROLES[1:])
    return f


def scores():
    """A chart: thin bars on one 0-1 axis, direct labels, a dashed threshold."""
    rows = [("min", 0.40), ("max", 0.80), ("mean", 0.65)]
    f = Figure("scores", 720, 150, "Minimum 0.40, maximum 0.80 and mean 0.65, against a threshold of 0.7.")
    x0, span = 120, 460
    y = 10
    for name, value in rows:
        f.text(0, y + 12, name, "logic", "title", TITLE)
        f.rect(x0, y, span, 14, "faint", stroke=False, opacity=0.35, rx=2)
        f.bar(x0, y, span * value, 14, "logic")
        f.text(x0 + span + 16, y + 12, f"{value:.2f}", "ink", "mono", SMALL)
        y += 34
    f.line([(x0 + span * 0.7, 4), (x0 + span * 0.7, y - 8)], "evidence", 1.5, dashed=True, arrow=False)
    f.text(x0 + span * 0.7 + 6, y + 8, "threshold 0.7", "evidence", "mono", SMALL)
    return f


def flow_animated():
    """An animation: a token carries the answer; boxes light up as it arrives."""
    end = 5.0
    f = Figure("flow-animated", 720, 110, "A request goes to the model, which answers 0.82; the rule decides true.",
               cycle=end + 0.6)
    cy = 50
    f.box(0, cy - 28, 120, 56, "ink", "request", "the input", lit=[(0.2, end)])
    f.box(170, cy - 28, 150, 56, "model", "ask model", [("P(yes) = 0.82", [(1.4, end)], True)], lit=[(1.0, end)])
    f.box(370, cy - 28, 130, 56, "logic", "≥ 0.7", [("true", [(2.6, end)], True)], lit=[(2.4, end)])
    f.box(550, cy - 28, 170, 56, "ink", "decision", [("true + trace", [(3.8, end)], True)], lit=[(3.6, end)])
    for points, role, start in (
        ([(120, cy), (167, cy)], "ink", 0.5),
        ([(320, cy), (367, cy)], "model", 1.9),
        ([(500, cy), (547, cy)], "logic", 3.1),
    ):
        f.line(points, role, anim=("lit", [(start, end)]))
        f.token(points, role, start, start + 0.5)
    return f


FIGURES = [flow, scores, flow_animated]
