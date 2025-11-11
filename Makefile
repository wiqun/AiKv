.PHONY: help build test clean fmt lint check run doc bench install dev

# Default target
help:
	@echo "AiKv Development Makefile"
	@echo ""
	@echo "Available targets:"
	@echo "  build         - Build the project (debug)"
	@echo "  release       - Build the project (release)"
	@echo "  test          - Run all tests"
	@echo "  test-unit     - Run unit tests only"
	@echo "  test-int      - Run integration tests only"
	@echo "  clean         - Clean build artifacts"
	@echo "  fmt           - Format code with rustfmt"
	@echo "  fmt-check     - Check code formatting"
	@echo "  lint          - Run clippy"
	@echo "  check         - Run all checks (fmt, lint, test)"
	@echo "  run           - Run the server"
	@echo "  run-release   - Run the server (release build)"
	@echo "  doc           - Generate and open documentation"
	@echo "  bench         - Run benchmarks"
	@echo "  coverage      - Generate test coverage report"
	@echo "  install       - Install development tools"
	@echo "  dev           - Start development mode with auto-reload"
	@echo "  audit         - Run security audit"
	@echo "  outdated      - Check for outdated dependencies"

# Build targets
build:
	cargo build

release:
	cargo build --release

# Test targets
test:
	cargo test --all-features

test-unit:
	cargo test --lib

test-int:
	cargo test --test '*'

test-verbose:
	cargo test --all-features -- --nocapture

# Clean
clean:
	cargo clean
	rm -rf target/

# Format
fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

# Lint
lint:
	cargo clippy --all-targets --all-features -- -D warnings

# Check all
check: fmt-check lint test
	@echo "All checks passed!"

# Run
run:
	cargo run

run-release:
	cargo run --release

# Documentation
doc:
	cargo doc --no-deps --all-features --open

doc-private:
	cargo doc --no-deps --all-features --document-private-items --open

# Benchmarks
bench:
	cargo bench

# Coverage
coverage:
	cargo tarpaulin --out Html --output-dir coverage/

# Install development tools
install:
	rustup component add rustfmt clippy
	cargo install cargo-watch cargo-edit cargo-audit cargo-outdated cargo-deny cargo-tarpaulin

# Development mode with auto-reload
dev:
	cargo watch -x 'run'

dev-test:
	cargo watch -x 'test'

# Security and maintenance
audit:
	cargo audit

deny:
	cargo deny check

outdated:
	cargo outdated

# Update dependencies
update:
	cargo update

# Examples
example:
	cargo run --example client_example

# CI simulation
ci: fmt-check lint test
	@echo "CI checks completed successfully!"
