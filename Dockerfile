# build image
FROM rust:1-slim AS builder

RUN apt-get update && \
    apt-get install -y pkg-config openssl libssl-dev

RUN groupadd --system nonroot && useradd --system --gid nonroot nonroot
USER nonroot:nonroot

WORKDIR /usr/local/node-taint-preserver
COPY ./Cargo.toml ./Cargo.toml
COPY ./src ./src
RUN cargo build --release

# runtime image
FROM debian:bookworm-slim
COPY --from=builder /usr/local/node-taint-preserver/target/release/node-taint-preserver /usr/local/bin/node-taint-preserver

# run as non-root user
RUN groupadd --system nonroot && useradd --system --gid nonroot nonroot
USER nonroot:nonroot

ENTRYPOINT ["/usr/local/bin/node-taint-preserver"]

