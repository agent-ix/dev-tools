# Include from a repository Makefile: `include docs/figures.mk`, then list the figure names.
# Each name NAME maps docs/diagrams/NAME.yaml to docs/images/NAME/ (figures/NAME.light.svg, ...).
# IXD is the ixd binary; IXD_PLAYER is the built player directory of an ix-diagrams clone.
FIGURES ?= example
IXD ?= ixd
IXD_PLAYER ?= ../ix-diagrams/packages/player/dist/site

.PHONY: figures figures-check
figures: $(FIGURES:%=figure-%)
figures-check: $(FIGURES:%=check-figure-%)

figure-%:
	$(IXD) build docs/diagrams/$*.yaml --out docs/images/$* --player-dist $(IXD_PLAYER)

check-figure-%:
	$(IXD) build docs/diagrams/$*.yaml --out docs/images/$* --player-dist $(IXD_PLAYER) --check
