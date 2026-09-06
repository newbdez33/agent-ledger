SKILL_DIR ?= $(HOME)/.claude/skills

.PHONY: build test install uninstall

build:
	cargo build --release

test:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

install:
	cargo install --path . --locked
	mkdir -p $(SKILL_DIR)
	ln -sfn $(CURDIR)/skill/ledger $(SKILL_DIR)/ledger
	@echo "installed: $$(command -v ledger) and $(SKILL_DIR)/ledger -> $(CURDIR)/skill/ledger"

uninstall:
	cargo uninstall agent-ledger
	rm -f $(SKILL_DIR)/ledger
