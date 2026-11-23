# Include from a repository Makefile (`include docs/figures.mk`) or copy the targets.
# docs/images/figures.json declares the figures; ixd writes <id>-light.svg, <id>-dark.svg and
# figures.manifest.json into docs/images. ixd has no registry release; install it from the private
# agent-ix/ix-diagrams repository (needs a git credential for agent-ix, e.g. `gh auth setup-git`):
#   CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --locked --git https://github.com/agent-ix/ix-diagrams ix-diagrams --bin ixd

IXD ?= ixd

.PHONY: figures
figures:
	$(IXD) build docs/images/figures.json --out docs/images

.PHONY: figures-check
figures-check:
	$(IXD) build docs/images/figures.json --out docs/images --check
