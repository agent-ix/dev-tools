.PHONY: install-tools check-install

install-tools:
	$(MAKE) -C tools/locked-build install-tools
	$(HOME)/.local/bin/locked-build check-install

check-install:
	locked-build check-install
