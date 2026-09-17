# Pénélope : mise à jour depuis les sources, sur la machine qui fait tourner le daemon.
#
#   make update   git pull puis cargo build --release
#   make deploy   update, puis remplace le binaire installé et redémarre le service
#
# Le binaire installé est celui que trouve le PATH (`command -v penelope`), sinon
# /usr/local/bin/penelope ; sudo n'est utilisé que si ce répertoire n'est pas inscriptible.
#
# macOS : avec SIGN_IDENTITY (certificat de signature de code, voir « Signature locale »
# dans docs/install-headless.md), `build` signe le binaire avec un identifiant fixe. Son
# exigence désignée ne change plus d'un build à l'autre : l'accès au Trousseau accordé une
# fois reste valable. Vide par défaut : CI et Linux inchangés.

CARGO           ?= cargo
BIN             := target/release/penelope
DEST            ?= $(shell command -v penelope 2>/dev/null || echo /usr/local/bin/penelope)
SUDO            := $(shell test -w "$(dir $(DEST))" || echo sudo)
SIGN_IDENTITY   ?=
SIGN_IDENTIFIER ?= io.github.edouard-claude.penelope
SIGN_FLAGS      ?=

.DEFAULT_GOAL := help
.PHONY: help pull build sign update install restart deploy test clean

help:
	@echo "make pull     git pull (avance rapide uniquement)"
	@echo "make build    cargo build --release (signé si SIGN_IDENTITY est défini)"
	@echo "make sign     signe $(BIN) avec SIGN_IDENTITY (macOS)"
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
	@if [ -n "$(SIGN_IDENTITY)" ]; then $(MAKE) --no-print-directory sign; fi

sign:
	@test -n "$(SIGN_IDENTITY)" || { echo "SIGN_IDENTITY est vide : voir « Signature locale » dans docs/install-headless.md"; exit 1; }
	codesign --force --timestamp=none --sign "$(SIGN_IDENTITY)" --identifier $(SIGN_IDENTIFIER) $(SIGN_FLAGS) "$(BIN)"
	codesign --verify --strict "$(BIN)"

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
