# Build
FROM rust:1.99-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
COPY examples ./examples
COPY tests ./tests
COPY data/wordlist.txt ./data/wordlist.txt
RUN cargo build --release --locked --bin scrabbler

# Run
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /app scrabbler
WORKDIR /app
COPY --from=build /app/target/release/scrabbler /usr/local/bin/scrabbler
COPY data/wordlist.txt /app/data/wordlist.txt
ENV WORDLIST=/app/data/wordlist.txt \
    DATA_DIR=/app/state \
    TOKEN_FILE=/app/state/token \
    RUST_LOG=info
RUN mkdir -p /app/state && chown scrabbler /app/state
USER scrabbler
VOLUME /app/state
ENTRYPOINT ["scrabbler"]
CMD ["run"]
