.PHONY: install-tools check-install

install-tools:
	$(MAKE) -C tools/ix-build install-tools
	$(HOME)/.local/bin/ix-build check-install

check-install:
	ix-build check-install
