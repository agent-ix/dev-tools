# Include from a repository Makefile (`include docs/figures.mk`) or copy the targets.
# docs/images/figures.json declares the figures; ixd writes <id>-light.svg, <id>-dark.svg and
# figures.manifest.json into docs/images. Install ixd from agent-ix/ix-diagrams:
#   cargo install --locked --git https://github.com/agent-ix/ix-diagrams ix-diagrams

IXD ?= ixd

.PHONY: figures
figures:
	$(IXD) build docs/images/figures.json --out docs/images

.PHONY: figures-check
figures-check:
	$(IXD) build docs/images/figures.json --out docs/images --check
