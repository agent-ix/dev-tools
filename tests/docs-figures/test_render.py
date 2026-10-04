from __future__ import annotations

import importlib.util
import shutil
import sys
import urllib.error
from pathlib import Path

import pytest

pytest.importorskip("fontTools")

SKILL = Path(__file__).parents[2] / "skills" / "docs-figures"
sys.path.insert(0, str(SKILL / "scripts"))
sys.dont_write_bytecode = True

import docs_figures  # noqa: E402

SPEC = importlib.util.spec_from_file_location("docs_figures_render", SKILL / "scripts" / "render.py")
assert SPEC is not None and SPEC.loader is not None
RENDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RENDER)


@pytest.fixture(scope="module", autouse=True)
def _style() -> None:
    try:
        docs_figures.use_style("ix-docs")
    except (urllib.error.URLError, TimeoutError) as error:
        pytest.skip(f"fonts unavailable: {error}")


@pytest.fixture
def figures(tmp_path: Path) -> Path:
    path = tmp_path / "figures.py"
    shutil.copy(SKILL / "assets" / "starter_figures.py", path)
    return path


def test_starter_renders_light_dark_and_still_svgs(figures: Path) -> None:
    assert RENDER.main([str(figures)]) == 0
    names = sorted(p.name for p in figures.parent.glob("*.svg"))
    assert names == [
        "flow-animated-dark.svg", "flow-animated-light.svg",
        "flow-animated-still-dark.svg", "flow-animated-still-light.svg",
        "flow-dark.svg", "flow-light.svg", "scores-dark.svg", "scores-light.svg",
    ]
    animated = (figures.parent / "flow-animated-dark.svg").read_text()
    still = (figures.parent / "flow-animated-still-dark.svg").read_text()
    assert "@keyframes" in animated and "prefers-reduced-motion" in animated
    assert "<style>" not in still
    assert "<text" not in animated  # every label is outlines


def test_rendering_is_deterministic_and_check_detects_stale_files(figures: Path) -> None:
    RENDER.main([str(figures)])
    first = {p.name: p.read_bytes() for p in figures.parent.glob("*.svg")}
    RENDER.main([str(figures)])
    assert {p.name: p.read_bytes() for p in figures.parent.glob("*.svg")} == first
    assert RENDER.main([str(figures), "--check"]) == 0
    stale = figures.parent / "scores-light.svg"
    stale.write_text(stale.read_text() + " ")
    assert RENDER.main([str(figures), "--check"]) == 1


def test_themes_use_their_own_palette(figures: Path) -> None:
    RENDER.main([str(figures)])
    light = (figures.parent / "flow-light.svg").read_text()
    dark = (figures.parent / "flow-dark.svg").read_text()
    assert docs_figures.THEMES["light"]["model"] in light and docs_figures.THEMES["light"]["model"] not in dark
    assert docs_figures.THEMES["dark"]["model"] in dark


def test_box_refuses_text_that_does_not_fit() -> None:
    figure = docs_figures.Figure("fit", 400, 100, "Fit check.")
    with pytest.raises(ValueError, match="needs"):
        figure.box(0, 0, 60, 56, "model", "a title far too long for the box")


def test_text_refuses_missing_glyphs_and_leaving_the_canvas() -> None:
    figure = docs_figures.Figure("glyphs", 400, 100, "Glyph check.")
    with pytest.raises(ValueError, match="no glyph"):
        figure.text(0, 20, "\U0001F600")
    with pytest.raises(ValueError, match="leaves the canvas"):
        figure.text(390, 20, "outside")


def test_animation_requires_a_cycle() -> None:
    figure = docs_figures.Figure("still", 400, 100, "No cycle.")
    with pytest.raises(ValueError, match="cycle"):
        figure.token([(0, 0), (10, 0)], "model", 0.1, 0.5)


def test_unknown_style_lists_available_styles() -> None:
    with pytest.raises(ValueError, match="ix-docs"):
        docs_figures.use_style("missing")
    docs_figures.use_style("ix-docs")


def test_pencil_style_sketches_strokes_deterministically(figures: Path, tmp_path: Path) -> None:
    out = tmp_path / "pencil"
    out.mkdir()
    try:
        assert RENDER.main([str(figures), "--style", "pencil", "--out", str(out)]) == 0
        first = {p.name: p.read_bytes() for p in out.glob("*.svg")}
        RENDER.main([str(figures), "--style", "pencil", "--out", str(out)])
        assert {p.name: p.read_bytes() for p in out.glob("*.svg")} == first
        flow = (out / "flow-light.svg").read_text()
        assert 'filter id="tremor"' in flow and "<marker" not in flow  # hand-drawn arrowheads
        assert " C" in flow  # strokes are curves, not straight lines
        assert docs_figures.THEMES["light"]["model"] in flow
    finally:
        docs_figures.use_style("ix-docs")


def test_clean_style_draws_no_sketch(figures: Path) -> None:
    RENDER.main([str(figures)])
    flow = (figures.parent / "flow-light.svg").read_text()
    assert "tremor" not in flow and "<marker" in flow


def test_missing_glyphs_fall_back_to_the_next_font() -> None:
    try:
        docs_figures.use_style("pencil")
        figure = docs_figures.Figure("fallback", 300, 60, "Fallback.")
        figure.text(0, 30, "x ≥ 0.7", "ink", "mono", 14)
        svg = figure.render("light")
        assert 'id="mono_' in svg and 'id="mono1_' in svg  # Kalam plus the Plex fallback for ≥
    finally:
        docs_figures.use_style("ix-docs")
