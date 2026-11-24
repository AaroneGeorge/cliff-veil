# cliff-veil Makefile.
# Every recipe sources scripts/env.sh (target dir, build jobs, RISC0_DEV_MODE=1, LEZ paths) and builds niced.

SHELL := /usr/bin/env bash
.SHELLFLAGS := -eu -o pipefail -c
ENV := source scripts/env.sh &&
NICE := nice -n 10
GUESTS := vesting vesting_fees token

.PHONY: help fmt fmt-check clippy wording lint test guests guests-release guests-release-network e2e ci hooks seq-dev-start seq-dev-stop \
        seq-pub-start seq-pub-stop deploy-local

help: ## List targets
	@grep -E '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-16s %s\n", $$1, $$2}'

fmt: ## Format all Rust code
	$(ENV) cargo fmt --all

fmt-check: ## Check formatting
	$(ENV) cargo fmt --all -- --check

clippy: ## Clippy on every crate and target, warnings are errors (guest build skipped)
	$(ENV) RISC0_SKIP_BUILD=1 $(NICE) cargo clippy --workspace --all-targets --locked -- -D warnings

wording: ## Wording check of every tracked file (scripts/hooks/wording.py)
	python3 -I scripts/hooks/wording.py --tree

lint: fmt-check clippy wording ## Formatting, clippy and wording

test: ## Unit and integration tests (dev mode, guest build skipped; end-to-end tests are `make e2e`)
	$(ENV) RISC0_SKIP_BUILD=1 $(NICE) cargo test --workspace --locked

guests: ## Build the guests with the local rzup toolchain into $$CARGO_TARGET_DIR/guests
	$(ENV) $(NICE) cargo build --locked -p vesting_methods && scripts/collect_guests.sh $(GUESTS)

guests-release: ## Reproducible guests of HEAD in the pinned image (local test ids), checked against program/IMAGE_IDS.expected
	$(ENV) scripts/guests_release.sh && scripts/check_image_ids.sh "$$B/build/guests-release/$$(git rev-parse HEAD)"

guests-release-network: ## As guests-release with the ids of IDS=deploy/ids-<network>.toml, checked against its [image_ids]
	$(ENV) d="$$(scripts/guests_release.sh --ids "$(IDS)" | sed -n 's/^release set: //p; s/ already complete$$//p' | tail -1)" \
	  && scripts/check_image_ids.sh "$$d" "$(IDS)"

e2e: ## End-to-end tests against the running dev sequencer (scripts/seq.sh dev start; CV_SEQ_RPC overrides)
	$(ENV) [ -n "$${CV_SEQ_RPC:-}" ] || scripts/seq.sh dev status
	$(ENV) $(NICE) e2e/run.sh

ci: lint test ## What the CI rust job runs (guests and e2e are separate jobs)

hooks: ## Use the repository's git hooks (identity, trailers, wording)
	git config core.hooksPath scripts/hooks

seq-dev-start: ## Start the dev-mode sequencer (RPC 3050)
	scripts/seq.sh dev start

seq-dev-stop: ## Stop the dev-mode sequencer
	scripts/seq.sh dev stop

seq-pub-start: ## Start the sequencer with dev mode unset (RPC 3051)
	scripts/seq.sh pub start

seq-pub-stop: ## Stop the sequencer with dev mode unset
	scripts/seq.sh pub stop

deploy-local: ## Build and deploy token, vesting_fees (+ Init) and vesting to the dev sequencer: deploy/ids-local-dev.toml
	scripts/deploy_local.sh dev
