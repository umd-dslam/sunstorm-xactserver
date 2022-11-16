FROM rust:1.65 AS builder
WORKDIR /home/nonroot

COPY . .

RUN apt-get update
RUN apt-get install -y cmake
RUN cargo build --locked --release

FROM debian:bullseye-slim
WORKDIR /data

COPY --from=builder /home/nonroot/target/release/xactserver /usr/local/bin

ENTRYPOINT ["/usr/local/bin/xactserver"]