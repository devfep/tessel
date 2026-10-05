# Image for repos that declare a tessel.toml (Tessel itself): the Rust toolchain, Node, pnpm and
# every dependency in the committed lockfiles, so the gate runs with the Internet off.
#
# The build context is the repository root (wrangler.jsonc: build_context ".."), because the
# lockfiles and manifests live there. The dependencies are cooked from Cargo.lock and
# pnpm-lock.yaml only: a source change does not rebuild them, a lockfile change does.
#
# Skipped on purpose: the wasm32 target and clippy. The gate is `cargo test` and `pnpm test`.

FROM rust:1.98.0-slim-trixie AS chef
# A separate CARGO_HOME keeps cargo-chef and its dependencies out of the registry the image ships.
RUN CARGO_HOME=/opt/chef-home cargo install cargo-chef --locked --version 0.1.78 --root /opt/chef
ENV PATH=/opt/chef/bin:$PATH
# Cargo fingerprints record paths, so the cook runs where the steward clones the repo.
WORKDIR /workspace

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path /recipe.json

FROM chef AS builder
ENV CARGO_TARGET_DIR=/opt/cargo-target \
	CARGO_INCREMENTAL=0 \
	CARGO_PROFILE_DEV_DEBUG=0
COPY --from=planner /recipe.json /recipe.json
# cook builds the test profile of every dependency; fetch also downloads the crates that only
# other platforms use, which `cargo fetch --locked --offline` in the install step expects.
RUN cargo chef cook --tests --workspace --locked --recipe-path /recipe.json \
	&& cargo fetch --locked

FROM node:22.23.3-trixie-slim
# procps provides /bin/kill, which the CLI's tests use to probe their daemons.
RUN apt-get update \
	&& apt-get install --yes --no-install-recommends ca-certificates git gcc libc6-dev procps \
	&& rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/rustup /usr/local/rustup
# The gate runs as the unprivileged node user, so what it writes to is owned by node.
COPY --from=builder --chown=node:node /usr/local/cargo /usr/local/cargo
COPY --from=builder --chown=node:node /opt/cargo-target /opt/cargo-target
# Keep these in step with TOOLCHAIN_ENV in src/container-step.ts (a test compares them).
ENV PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
	CARGO_HOME=/usr/local/cargo \
	RUSTUP_HOME=/usr/local/rustup \
	CARGO_TARGET_DIR=/opt/cargo-target \
	CARGO_INCREMENTAL=0 \
	CARGO_PROFILE_DEV_DEBUG=0 \
	npm_config_store_dir=/opt/pnpm-store
RUN npm install --global pnpm@12.8.1 \
	&& mkdir /workspace /opt/pnpm-store \
	&& chown node:node /workspace /opt/pnpm-store
COPY --chown=node:node tessel-steward/package.json tessel-steward/pnpm-lock.yaml tessel-steward/pnpm-workspace.yaml /tmp/steward-lock/
USER node
RUN cd /tmp/steward-lock && pnpm fetch && rm -rf /tmp/steward-lock
WORKDIR /workspace
CMD ["sleep", "infinity"]
