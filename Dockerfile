FROM rust:1.70 AS builder
WORKDIR /home/nonroot

RUN apt-get update
RUN apt-get install -y cmake protobuf-compiler

COPY Cargo.toml .
COPY Cargo.lock .
COPY xactserver/Cargo.toml ./xactserver/Cargo.toml
RUN mkdir xactserver/src \
    && touch xactserver/src/lib.rs \
    && cargo build --locked --release

COPY . .
RUN cargo build --locked --release

FROM debian:bullseye-slim
WORKDIR /data

COPY --from=builder /home/nonroot/target/release/xactserver /usr/local/bin

ENTRYPOINT ["/usr/local/bin/xactserver"]