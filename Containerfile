FROM rust:1-trixie AS build
RUN apt-get update && apt-get install -y --no-install-recommends libclang-dev cmake pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY vendor ./vendor
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libasound2t64 libpulse0 libssl3t64 libstdc++6 libgomp1 && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/ttradiobot /usr/local/bin/ttradiobot
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ttradiobot", "/config/config.toml"]
