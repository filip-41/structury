# structury: the core crate and the JSON codec.
.DEFAULT_GOAL := check
CARGO ?= cargo
CARGO_TARGET_DIR ?= $(CURDIR)/target
# The native `lint` is single-architecture: on this aarch64 host it never sees the
# cfg-gated x86_64 kernels. `lint-cross` covers them; CI's clippy matrix covers both.
CROSS_TARGET ?= x86_64-unknown-linux-gnu
MIRI_TOOLCHAIN ?= nightly-2026-07-15
export CARGO_TARGET_DIR

.PHONY: check fmt-check lint lint-cross test doc miri fuzz-check gate help

check: ## cargo check --workspace --all-targets
	$(CARGO) check --workspace --all-targets --manifest-path $(CURDIR)/Cargo.toml

fmt-check: ## cargo fmt --check
	$(CARGO) fmt --check

lint: ## cargo clippy --workspace --all-targets, pedantic -D warnings
	$(CARGO) clippy --workspace --all-targets --manifest-path $(CURDIR)/Cargo.toml -- -D warnings

lint-cross: ## clippy for $(CROSS_TARGET) (the arch native lint does not build)
	@if rustup target list --installed 2>/dev/null | grep -qx '$(CROSS_TARGET)'; then \
		$(CARGO) clippy --workspace --all-targets --target $(CROSS_TARGET) --manifest-path $(CURDIR)/Cargo.toml -- -D warnings; \
	else \
		echo "lint-cross: $(CROSS_TARGET) not installed; skipped (CI's clippy matrix covers both arches)"; \
	fi

test: ## cargo test --workspace < /dev/null
	$(CARGO) test --workspace --manifest-path $(CURDIR)/Cargo.toml < /dev/null

doc: ## cargo doc --workspace --no-deps, warnings denied
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked --manifest-path $(CURDIR)/Cargo.toml

miri: ## cargo miri the unsafe byte_scan kernels (strict provenance); NOT in gate (slow)
	MIRIFLAGS="-Zmiri-strict-provenance" $(CARGO) +$(MIRI_TOOLCHAIN) miri test -p structury --features byte-scan byte_scan

fuzz-check: ## cargo check the fuzz targets (excluded from the workspace)
	$(CARGO) check --manifest-path $(CURDIR)/json/fuzz/Cargo.toml

gate: ## fmt-check, lint(+cross), test, doc, fuzz-check
	$(MAKE) --no-print-directory -j1 fmt-check lint lint-cross test doc fuzz-check

help: ## list targets
	@awk 'BEGIN {FS = ":.*## "; printf "structury targets (default: check):\n\n"} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-22s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
