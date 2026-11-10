FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 1000 --create-home scanner \
    && mkdir -p /scans /config \
    && chown scanner /scans
COPY --from=build /src/target/release/hp-scan-to-mac /usr/local/bin/hp-scan-to-mac
USER scanner
ENV HP_SCAN_OUTPUT=/scans \
    HP_SCAN_NAME=Scanner \
    HEALTH_LISTEN=0.0.0.0:8090
VOLUME ["/scans"]
EXPOSE 8090
HEALTHCHECK --interval=60s --timeout=10s --start-period=30s \
    CMD ["hp-scan-to-mac", "healthcheck"]
ENTRYPOINT ["hp-scan-to-mac"]
CMD ["run"]
