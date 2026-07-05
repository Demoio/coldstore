.PHONY: build build-debug fmt fmt-check clippy test unit test-unit vtl-unit vtl-check check check-all check-safe clean help install-hooks install-tools setup lint run-metadata run-gateway run-scheduler run-cache run-tape run-node run-node-no-tape

.DEFAULT_GOAL := help

BUILD_MODE ?= release

build:
	@echo "Building all crates ($(BUILD_MODE))..."
	@if [ "$(BUILD_MODE)" = "release" ]; then cargo build --release; else cargo build; fi

build-debug:
	@$(MAKE) BUILD_MODE=debug build

fmt:
	@cargo fmt --all
fmt-check:
	@cargo fmt --all -- --check
clippy:
	@cargo clippy --all-targets --all-features -- -D warnings
test:
	@cargo test --workspace --all-features
unit test-unit:
	@cargo test --workspace --lib --bins
vtl-unit:
	@cargo test -p coldstore-vtl --lib --tests
vtl-check:
	@cargo check -p coldstore-vtl
check:
	@cargo check --all-targets --all-features
check-all: fmt-check clippy test
check-safe: fmt-check clippy unit vtl-unit build-debug
	@echo "Safe verification passed (fmt/clippy/unit/vtl-unit/build only)."
clean:
	@cargo clean

run-metadata:
	@cargo run -p coldstore-metadata
run-gateway:
	@cargo run -p coldstore-gateway
run-scheduler:
	@cargo run -p coldstore-scheduler
run-cache:
	@cargo run -p coldstore-cache
run-tape:
	@cargo run -p coldstore-tape
run-node:
	@cargo run -p coldstore-node
run-node-no-tape:
	@cargo run -p coldstore-node -- --config configs/single-node-no-tape.yaml

lint: fmt-check clippy check unit
	@echo "All lint checks passed (unit-only)."

install-hooks:
	@cp scripts/pre-commit .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit
	@echo "Pre-commit hook installed."

install-tools:
	@rustup component add rustfmt clippy
	@echo "protobuf-compiler required: apt install protobuf-compiler"

setup: install-tools install-hooks
	@echo "Development environment ready."

help:
	@echo "ColdStore Workspace Makefile"
	@echo ""
	@echo "Build:   build build-debug clean"
	@echo "Quality: fmt fmt-check clippy test unit vtl-unit vtl-check check check-all check-safe lint"
	@echo "Run:     run-node run-node-no-tape run-metadata run-gateway run-scheduler run-cache run-tape"
	@echo "Setup:   setup install-tools install-hooks"
