# Compile, then ship only the binary. k8s/api.yaml relies on exactly two things from this
# image: it listens on 8080 and answers /healthz.
FROM rust:1-slim-bookworm AS build
WORKDIR /app

# Dependencies are built against a stub main so this layer survives every change that
# touches only src/ — without it each push recompiles the whole tree from scratch.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src

COPY src ./src
# The stub's artifacts are newer than the real sources cargo just replaced, so without a
# touch cargo decides there is nothing to rebuild and ships the stub.
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
# Non-root is not required by the baseline policy, but nothing here needs root.
RUN useradd --system --uid 10001 monke
USER 10001
COPY --from=build /app/target/release/monke-api /usr/local/bin/monke-api
EXPOSE 8080
CMD ["monke-api"]
