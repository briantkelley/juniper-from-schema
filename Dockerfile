FROM debian:11.2-slim AS dev

ARG RUST_VERSION=1.59.0
ARG NODE_VERSION=16.14.2

# install dependencies required for build
RUN set -ex \
    && apt-get -y update && apt-get install -y \
    curl \
    && curl -fsSL https://deb.nodesource.com/setup_${NODE_VERSION}} | bash - \
    && apt-get -y update && apt-get install -y \
    nodejs \
    npm \
    git \
    bash-completion \
    && rm -rf /var/lib/apt/lists/* \
    \
    # add non root user (`jfs` for `juniper-from-schema`)
    && useradd -ms /bin/bash jfs
# switch to non root user for better security
USER jfs

# install rust toolchain
RUN curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    && bash -c "source $HOME/.cargo/env \
    && rustup default ${RUST_VERSION} \
    && rustup component add \
    rust-analysis \
    rust-src \
    rls"