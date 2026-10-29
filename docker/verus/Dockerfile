# Verus（証明つき Rust）の道具。Verus の安定版は x86_64 Linux のバイナリしかないので amd64 で作る
# （arm64 のホストではエミュレーションになり遅い）。バイナリは glibc 2.39 以上が要るので Ubuntu 24.04。
# リリースの zip は版と sha256 の両方で固定する。
FROM ubuntu:24.04

ARG VERUS_VER=0.2026.10.04.426d8b0
ARG VERUS_SHA256=624018662868563a7bec0dd07be5e4dd0c3d61124b4b55105765884bf2d4d1a0
# この Verus のリリースが作られた Rust の版（version.json）
ARG RUST_VER=1.98.1

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl unzip gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/opt/verus:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_VER}

RUN curl -sSfL -o /tmp/verus.zip \
      "https://github.com/verus-lang/verus/releases/download/release%2F${VERUS_VER}/verus-${VERUS_VER}-x86-linux.zip" \
 && echo "${VERUS_SHA256}  /tmp/verus.zip" | sha256sum -c - \
 && unzip -q /tmp/verus.zip -d /tmp/v && mv /tmp/v/verus-x86-linux /opt/verus && rm -rf /tmp/verus.zip /tmp/v

WORKDIR /work
