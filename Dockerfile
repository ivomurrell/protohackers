FROM rust:1.64 as builder

WORKDIR /usr/src/protohackers
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/protohackers/target \
    cargo install --profile release --locked --path .


FROM debian:bullseye-slim

COPY --from=builder /usr/local/cargo/bin/protohackers /usr/local/bin/protohackers
CMD ["protohackers"]
