# Compile, then ship only the binary. k8s/api.yaml relies on exactly two things from this
# image: it listens on 8080 and answers /healthz.
#
# Built from the repo ROOT, not apps/api — this crate depends on apps/common, and a path
# dependency outside the build context does not exist as far as the build is concerned. The
# context and this file's location are passed explicitly in
# .github/workflows/build-and-deploy.yml.
FROM rust:1-slim-bookworm AS build
WORKDIR /app

# Dependencies are built against a stub main so this layer survives every change that touches
# only src/ — without it each push recompiles the whole tree from scratch.
#
# Every workspace member needs a manifest *and* a stub entry point here, not just the two this
# image builds: cargo loads the whole workspace before it builds any of it, and a member whose
# src/main.rs is missing fails that load.
COPY Cargo.toml Cargo.lock ./
COPY apps/api/Cargo.toml apps/api/
COPY apps/common/Cargo.toml apps/common/
COPY apps/worker/Cargo.toml apps/worker/
RUN mkdir -p apps/api/src apps/common/src apps/worker/src \
    && echo 'fn main() {}' > apps/api/src/main.rs \
    && echo 'fn main() {}' > apps/worker/src/main.rs \
    && touch apps/common/src/lib.rs \
    && cargo build --release -p monke-api

# sqlx::migrate! embeds these at compile time, so they must be present for the real build.
# The stub sources are overwritten in place rather than removed: deleting apps/worker/src
# would break the workspace load again, and it is never built here anyway.
COPY apps/common ./apps/common
COPY apps/api ./apps/api
# The stub's artifacts are newer than the real sources cargo just replaced, so without a touch
# cargo decides there is nothing to rebuild and ships the stub.
RUN touch apps/common/src/lib.rs apps/api/src/main.rs \
    && cargo build --release -p monke-api

FROM debian:bookworm-slim
# reqwest's rustls backend loads the system root store when it builds a client, even for an
# all-HTTP target like today's Garage endpoint — without this package that load finds
# nothing and panics.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Non-root is not required by the baseline policy, but nothing here needs root.
RUN useradd --system --uid 10001 monke
USER 10001
COPY --from=build /app/target/release/monke-api /usr/local/bin/monke-api
EXPOSE 8080
CMD ["monke-api"]
