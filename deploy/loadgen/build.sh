#!/usr/bin/env bash
# 构建 loadgen; --check 只跑 fmt/clippy 门禁.
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

usage() {
    printf '用法: %s [--check]\n' "$(basename "$0")" >&2
    printf '  --check  仅执行 cargo fmt --check 与 clippy (-D warnings)\n' >&2
    exit 2
}

command -v cargo >/dev/null 2>&1 || {
    printf 'error: required command not found: cargo\n' >&2
    exit 1
}

mode="build"
for arg in "$@"; do
    case "$arg" in
        --check) mode="check" ;;
        -h|--help) usage ;;
        *) usage ;;
    esac
done

cd "$SCRIPT_DIR"
if [[ "$mode" == "check" ]]; then
    cargo fmt --check
    RUSTFLAGS='-D warnings' cargo clippy --all-targets
    printf 'loadgen 门禁通过 (fmt + clippy)\n'
else
    cargo build --release
    printf '构建完成: %s/target/release/loadgen\n' "$SCRIPT_DIR"
fi
