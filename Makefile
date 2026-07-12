PREFIX ?= $(HOME)/.local
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user

.PHONY: build test install install-service uninstall

build:
	cargo build --release

test:
	cargo test --all-targets

install: build
	install -Dm755 target/release/waywarm "$(PREFIX)/bin/waywarm"

install-service: install
	install -Dm644 packaging/waywarm.service "$(SYSTEMD_USER_DIR)/waywarm.service"
	systemctl --user daemon-reload

uninstall:
	-systemctl --user disable --now waywarm.service
	rm -f "$(PREFIX)/bin/waywarm" "$(SYSTEMD_USER_DIR)/waywarm.service"
	systemctl --user daemon-reload
