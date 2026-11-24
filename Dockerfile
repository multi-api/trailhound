# Build
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked

# Run
FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /data trailhound \
    && mkdir /data && chown trailhound:trailhound /data
COPY --from=build /src/target/release/trailhound /usr/local/bin/trailhound
USER trailhound
ENV TRAILHOUND_DATA_DIR=/data \
    TRAILHOUND_ADDR=0.0.0.0:7700
VOLUME /data
EXPOSE 7700
ENTRYPOINT ["trailhound"]
