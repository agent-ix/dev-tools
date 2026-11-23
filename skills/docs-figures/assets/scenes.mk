# Secondary path: YAML scenes. Include from a repository Makefile and list the scene names.
# Each name NAME maps docs/diagrams/NAME.yaml to docs/images/NAME/ (figures/NAME.light.svg, ...).
# IXD is the ixd binary; IXD_PLAYER is the built player directory of an ix-diagrams clone.
SCENES ?= example
IXD ?= ixd
IXD_PLAYER ?= ../ix-diagrams/packages/player/dist/site

.PHONY: scenes scenes-check
scenes: $(SCENES:%=scene-%)
scenes-check: $(SCENES:%=check-scene-%)

scene-%:
	$(IXD) build docs/diagrams/$*.yaml --out docs/images/$* --player-dist $(IXD_PLAYER)

check-scene-%:
	$(IXD) build docs/diagrams/$*.yaml --out docs/images/$* --player-dist $(IXD_PLAYER) --check
