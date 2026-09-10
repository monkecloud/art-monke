# The transcode worker. No port and no probe — k8s/worker.yaml runs it as a queue consumer,
# and restart-on-crash is the whole health story.
#
# Built from the repo ROOT for the same reason apps/api/Dockerfile is: this crate depends on
# apps/common, which is outside apps/worker.
FROM rust:1-slim-bookworm AS build
WORKDIR /app

# Same stub-main dependency cache as the api image, priming this crate's dependency set.
COPY Cargo.toml Cargo.lock ./
COPY apps/api/Cargo.toml apps/api/
COPY apps/common/Cargo.toml apps/common/
COPY apps/worker/Cargo.toml apps/worker/
RUN mkdir -p apps/api/src apps/common/src apps/worker/src \
    && echo 'fn main() {}' > apps/api/src/main.rs \
    && echo 'fn main() {}' > apps/worker/src/main.rs \
    && touch apps/common/src/lib.rs \
    && cargo build --release -p monke-worker

COPY apps/common ./apps/common
COPY apps/worker ./apps/worker
RUN touch apps/common/src/lib.rs apps/worker/src/main.rs \
    && cargo build --release -p monke-worker

FROM debian:bookworm-slim
# ffmpeg brings ffprobe with it. Debian bookworm's build includes the native `aac` encoder,
# which is license-free — libfdk_aac would mean building ffmpeg from source rather than
# installing it. See the encoder note in src/main.rs: the native encoder clamps at roughly
# 224kbps, which is what the top tier is named and sized for. Its weakness relative to
# libfdk_aac is at the *bottom* of the ladder, not the top.
#
# ca-certificates for the same reason as the api image: reqwest's rustls backend loads the
# system root store when the client is built, and panics if there is nothing there.
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 monke
USER 10001
# Scratch space is /tmp/scratch, created by the worker itself on every job. /tmp is 1777 in
# the base image, so uid 10001 can write there without a PVC or a mounted volume.
COPY --from=build /app/target/release/monke-worker /usr/local/bin/monke-worker
CMD ["monke-worker"]
