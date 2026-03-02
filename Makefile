.PHONY: build fmt clippy test clean check help install-hooks lint

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
	@cargo test --all-features
check:
	@cargo check --all-targets --all-features
check-all: fmt-check clippy test
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

lint: fmt-check clippy check test
	@echo "All lint checks passed."

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
	@echo "Quality: fmt fmt-check clippy test check check-all lint"
	@echo "Run:     run-metadata run-gateway run-scheduler run-cache run-tape"
	@echo "Setup:   setup install-tools install-hooks"
