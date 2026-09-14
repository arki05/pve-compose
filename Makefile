PREFIX ?= /usr
DESTDIR ?=
CARGO ?= cargo

.PHONY: build test check deb install clean

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --locked

check:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets --locked -- -D warnings
	$(CARGO) test --locked

install: build
	install -D -m 0755 target/release/pve-compose $(DESTDIR)$(PREFIX)/sbin/pve-compose
	install -D -m 0644 pve-compose.service $(DESTDIR)$(PREFIX)/lib/systemd/system/pve-compose.service
	install -D -m 0644 prefixes/compose.yaml $(DESTDIR)$(PREFIX)/share/pve-meta/prefixes/compose.yaml
	install -D -m 0644 pve-compose.cfg.example $(DESTDIR)$(PREFIX)/share/doc/pve-compose/pve-compose.cfg.example

deb:
	dpkg-buildpackage -b -us -uc

clean:
	$(CARGO) clean
	rm -rf debian/pve-compose debian/.debhelper debian/files debian/*.substvars debian/*.log debian/debhelper-build-stamp
