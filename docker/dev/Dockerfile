# ビルドとテストを走らせるイメージ。ホストには docker 以外何も入れない。
# テストは MySQL のコンテナを立てるので、docker CLI を入れ、ホストの docker ソケットを渡して使う。
FROM docker:27.4.0-cli AS docker-cli

FROM rust:1.98.1-bookworm
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
RUN rustup component add clippy rustfmt
WORKDIR /work
