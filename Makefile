# ホストには docker 以外何も入れない。CI（GitHub Actions）も同じターゲットを走らせる。
DEV      := ai-yagni-data-rust-dev
VERUS    := ai-yagni-data-verus
NAME     := ai-yagni-data-sdk-rust
SOCK     := -v /var/run/docker.sock:/var/run/docker.sock
CACHE    := -v ai-yagni-data-cargo:/usr/local/cargo/registry -v ai-yagni-data-cargo-git:/usr/local/cargo/git -v $(NAME)-target:/work/target
RUN      := docker run --rm $(SOCK) $(CACHE) -v $(CURDIR):/work -w /work $(DEV)

.DEFAULT_GOAL := help
.PHONY: help image verus-image build test lint fmt verify ci sh

help: ## このヘルプ
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

image: ## ビルドとテストのイメージを作る
	docker build -q -t $(DEV) docker/dev >/dev/null

verus-image: ## Verus のイメージを作る（amd64）
	docker build -q --platform linux/amd64 -t $(VERUS) docker/verus >/dev/null

build: image ## ビルド
	$(RUN) cargo build --all-targets --locked

test: image ## テスト（MySQL のコンテナを立てる）
	$(RUN) sh -c 'cargo build -q --locked --bin with-mysql && target/debug/with-mysql cargo test --locked'

lint: image ## clippy と rustfmt
	$(RUN) sh -c 'cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings'

fmt: image ## 整形する
	$(RUN) cargo fmt --all

verify: verus-image ## Verus で証明を確かめる（verified/）
	docker run --rm --platform linux/amd64 -v ai-yagni-data-verus-cargo:/opt/cargo/registry -v ai-yagni-data-verus-git:/opt/cargo/git \
	  -v $(CURDIR):/work -w /work/verified $(VERUS) cargo verus verify --target-dir /tmp/verus-target

ci: lint test verify ## CI で走らせるもの全部

sh: image ## 開発用のシェル
	docker run --rm -it $(SOCK) $(CACHE) -v $(CURDIR):/work -w /work $(DEV) bash
