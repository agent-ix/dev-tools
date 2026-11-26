CARGO ?= cargo
PREFIX ?= $(HOME)/.local

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

install:
	$(CARGO) install --locked --path . --root $(PREFIX)

install-tools:
	$(CARGO) run --locked --release -- install-tools

clean:
	$(CARGO) clean
