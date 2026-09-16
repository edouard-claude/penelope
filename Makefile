# Pénélope : mise à jour depuis les sources, sur la machine qui fait tourner le daemon.
#
#   make update   git pull puis cargo build --release
#   make deploy   update, puis remplace le binaire installé et redémarre le service
#
# Le binaire installé est celui que trouve le PATH (`command -v penelope`), sinon
# /usr/local/bin/penelope ; sudo n'est utilisé que si ce répertoire n'est pas inscriptible.

CARGO   ?= cargo
BIN     := target/release/penelope
DEST    ?= $(shell command -v penelope 2>/dev/null || echo /usr/local/bin/penelope)
SUDO    := $(shell test -w "$(dir $(DEST))" || echo sudo)

.DEFAULT_GOAL := help
.PHONY: help pull build update install restart deploy test clean

help:
	@echo "make pull     git pull (avance rapide uniquement)"
	@echo "make build    cargo build --release"
	@echo "make update   pull + build"
	@echo "make install  copie $(BIN) vers $(DEST)"
	@echo "make restart  penelope restart (le service repart avec le nouveau binaire)"
	@echo "make deploy   update + install + restart"
	@echo "make test     tests de tout le workspace"
	@echo "make clean    cargo clean (libère plusieurs Go)"

pull:
	git pull --ff-only

build:
	$(CARGO) build --release --locked

update: pull build

install: build
	@if [ "$(BIN)" -ef "$(DEST)" ]; then \
		echo "$(DEST) est déjà le binaire compilé : pas de copie"; \
	else \
		$(SUDO) install -d "$(dir $(DEST))" && \
		$(SUDO) install -m 0755 $(BIN) "$(DEST)"; \
	fi
	@"$(DEST)" --version

restart:
	"$(DEST)" restart

deploy: update install restart

test:
	$(CARGO) test --workspace --locked

clean:
	$(CARGO) clean
