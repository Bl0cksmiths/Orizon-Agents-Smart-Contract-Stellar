SHELL := /usr/bin/env bash

.PHONY: check build test fmt clippy clean deploy-test deploy-main deploy-escrow-v2 ttl-check ttl-extend

check:
	cargo check --all

build:
	stellar contract build

test:
	cargo test --all

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all -- -D warnings

clean:
	cargo clean

deploy-test:
	bash scripts/deploy_testnet.sh

deploy-main:
	CONFIRM_MAINNET=yes NETWORK=mainnet bash scripts/deploy_testnet.sh

# Deploys ONLY PaymentEscrow v2 on testnet, against the registry and asset
# SAC already in addresses.json. SETTLER is required; SOURCE defaults to admin.
deploy-escrow-v2:
	@test -n "$(SETTLER)" || { echo "usage: make deploy-escrow-v2 SETTLER=G... [SOURCE=admin]"; exit 1; }
	SETTLER="$(SETTLER)" SOURCE="$(or $(SOURCE),admin)" bash scripts/deploy_escrow_v2.sh

# Storage lifetimes (D-083). ttl-check only reads; ttl-extend pays rent from
# SOURCE, a funded throwaway identity (no contract role needed).
ttl-check:
	python3 scripts/extend_ttl.py --quiet

ttl-extend:
	@test -n "$(SOURCE)" || { echo "usage: make ttl-extend SOURCE=ttl-keeper"; exit 1; }
	python3 scripts/extend_ttl.py --apply --source "$(SOURCE)"
