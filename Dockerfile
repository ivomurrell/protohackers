FROM rust:1.64 as builder

WORKDIR /usr/src/protohackers
COPY . .

RUN cargo install --profile release --locked --path .


FROM debian:bullseye-slim

COPY --from=builder /usr/local/cargo/bin/protohackers /usr/local/bin/protohackers
CMD ["protohackers"]
