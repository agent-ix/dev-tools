CARGO ?= cargo
PREFIX ?= $(HOME)/.local
ROLLOUT_OWNER ?=

.PHONY: build fmt fmt-check lint test ci install install-tools clean

build:
	$(CARGO) build --locked --release

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

lint:
	$(CARGO) clippy --locked --all-targets -- -D warnings

test:
	$(CARGO) test --locked

ci: fmt-check lint test

install: install-tools

install-tools:
	CARGO_TARGET_DIR=$(CURDIR)/target $(CARGO) build --locked --release -j 4
	$(CURDIR)/target/release/ix-build install-tools --owner "$(ROLLOUT_OWNER)"

clean:
	$(CARGO) clean
