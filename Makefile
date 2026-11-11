# 提交前必须全绿的一条命令：make quality
.PHONY: fmt fmt-check lint test check quality clean

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

check:
	cargo check --workspace --all-targets

quality: fmt-check lint test

clean:
	cargo clean
