# Pénélope : mise à jour depuis les sources, sur la machine qui fait tourner le daemon.
#
#   make update   git pull puis cargo build --release
#   make deploy   update, puis copie le binaire au chemin stable et redémarre le service
#
# Chemin stable (issue #36) : le service lance toujours le même fichier, que les mises à
# jour remplacent (`make deploy` comme `/upgrade install`) ; il ne lance jamais
# `target/release`. DEST vaut le programme du LaunchAgent s'il est déjà stable, sinon le
# `penelope` du PATH hors `target/` (sans service), sinon `$(INSTALL_DIR)/penelope`.
# sudo n'est utilisé que si ce répertoire n'est pas inscriptible.
#
# macOS : avec SIGN_IDENTITY (certificat de signature de code, voir « Signature locale »
# dans docs/install-headless.md), `build` signe le binaire avec un identifiant fixe. Son
# exigence désignée ne change plus d'un build à l'autre : l'accès au Trousseau accordé une
# fois reste valable. Vide par défaut : CI et Linux inchangés.

CARGO           ?= cargo
BIN             := target/release/penelope
SERVICE_PLIST   := $(HOME)/Library/LaunchAgents/com.penelope.daemon.plist
INSTALL_DIR     ?= $(HOME)/.local/bin
LAUNCHED        := $(shell plutil -extract ProgramArguments.0 raw "$(SERVICE_PLIST)" 2>/dev/null)
ON_PATH         := $(shell command -v penelope 2>/dev/null)
stable           = $(if $(1),$(if $(findstring /target/,$(or $(shell realpath "$(1)" 2>/dev/null),$(1))),,$(1)))
DEST            ?= $(or $(call stable,$(LAUNCHED)),$(if $(LAUNCHED),,$(call stable,$(ON_PATH))),$(INSTALL_DIR)/penelope)
SUDO            := $(shell d="$(dir $(DEST))"; while [ ! -d "$$d" ]; do d=$$(dirname "$$d"); done; test -w "$$d" || echo sudo)
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
	@echo "make restart  penelope restart (migre le service vers $(DEST) si besoin)"
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
	@case "$(DEST)" in */target/*) echo "DEST=$(DEST) n'est pas un chemin stable"; exit 1;; esac
	$(SUDO) install -d "$(dir $(DEST))"
	@# Un lien vers target/release est remplacé par une copie : le service ne suit plus le dépôt.
	@if [ -L "$(DEST)" ]; then $(SUDO) rm -f "$(DEST)"; fi
	$(SUDO) install -m 0755 $(BIN) "$(DEST)"
	@"$(DEST)" --version

# Le service est rechargé ici, depuis ce shell, hors du job du daemon : c'est le seul
# endroit où son fichier change (migration vers le chemin stable).
restart:
	@rm -f "$(SERVICE_PLIST).sources"
	@if [ -f "$(SERVICE_PLIST)" ] && [ "$(LAUNCHED)" != "$(DEST)" ]; then \
		echo "Le service lance $(LAUNCHED) : il lancera désormais $(DEST)"; \
		"$(DEST)" uninstall && "$(DEST)" install; \
	else \
		"$(DEST)" restart; \
	fi

deploy: update install restart

test:
	$(CARGO) test --workspace --locked

clean:
	$(CARGO) clean
