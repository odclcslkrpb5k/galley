# Build environment for gbk-markdown (docs/plan.md §8).
#
# The binary built here runs on the host, which has the same libgtk-4 (4.22.4) and Pango (1.57).
# Every download is pinned by version and sha256. The tag scripts/check uses is a hash of this file
# and rust-toolchain.toml, so changing either builds a new image.
FROM ubuntu:26.04@sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7

ARG DEBIAN_FRONTEND=noninteractive
# xvfb, mesa and fonts are for the smoke run (gate step 8); jq is for the gate's checks; git is
# how cargo-deny fetches the advisory database.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config libgtk-4-dev curl ca-certificates jq git \
      xvfb xauth libgl1-mesa-dri libegl-mesa0 fonts-dejavu-core librsvg2-common \
      desktop-file-utils \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/rustup \
    CARGO_HOME=/cargo \
    PATH=/cargo/bin:/usr/local/bin:/usr/bin:/bin

# rustup 1.29.1, then the toolchain named in rust-toolchain.toml.
RUN curl -fsSL -o /tmp/rustup-init \
      https://static.rust-lang.org/rustup/archive/1.29.1/x86_64-unknown-linux-gnu/rustup-init \
 && echo "dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
 && rm /tmp/rustup-init
COPY rust-toolchain.toml /tmp/toolchain/rust-toolchain.toml
# Also the default, so tools run outside /work (cargo deny, nextest) find the same toolchain.
RUN cd /tmp/toolchain && rustup toolchain install \
 && rustup default "$(rustup show active-toolchain | cut -d' ' -f1)" \
 && rustc --version && cargo clippy --version

# Prebuilt tools (Substitute for `cargo install`: minutes faster, and pinned by hash).
RUN cd /tmp \
 && curl -fsSL -o deny.tgz https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz \
 && echo "9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f  deny.tgz" | sha256sum -c - \
 && tar xzf deny.tgz --strip-components=1 -C /usr/local/bin cargo-deny-0.20.2-x86_64-unknown-linux-musl/cargo-deny \
 && curl -fsSL -o nextest.tgz https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.146/cargo-nextest-0.9.146-x86_64-unknown-linux-gnu.tar.gz \
 && echo "682c21b777c333e96fd532e114d3a5a894e0729ab88d94c0a9f20f8419695428  nextest.tgz" | sha256sum -c - \
 && tar xzf nextest.tgz -C /usr/local/bin cargo-nextest \
 && curl -fsSL -o mutants.tgz https://github.com/sourcefrog/cargo-mutants/releases/download/v27.1.0/cargo-mutants-x86_64-unknown-linux-gnu.tar.gz \
 && echo "dfe6dc37d0342c891d2829b5a695aa57c2d0edecef7e7d0399a30cc6e206411e  mutants.tgz" | sha256sum -c - \
 && tar xzf mutants.tgz -C /usr/local/bin cargo-mutants \
 && rm -f deny.tgz nextest.tgz mutants.tgz \
 && cargo deny --version && cargo nextest --version && cargo mutants --version

# The build runs as the host user. Caches are named volumes mounted on these directories.
RUN mkdir -p /cargo/registry /cargo/git /cargo/advisory-dbs \
 && chmod 1777 /cargo /cargo/registry /cargo/git /cargo/advisory-dbs \
 && chmod -R a+rX /rustup /cargo/bin
