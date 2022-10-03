FROM rust:1.64 as builder

WORKDIR /usr/src/protohackers
COPY . .

RUN cargo install --profile release --locked --path 00


FROM debian:buster-slim

COPY --from=builder /usr/local/cargo/bin /usr/local/bin
