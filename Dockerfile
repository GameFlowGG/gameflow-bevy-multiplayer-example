# Game server image for GameFlow.
#
# GameFlow builds from the uploaded zip, and a Dockerfile at the root of that
# zip wins over any engine template. That is what makes a Rust server possible
# without changing anything on the platform side.
#
# cargo-chef exists here for one reason: a cold Bevy build is minutes long, and
# without splitting the dependency layer every single code change would pay that
# cost again.

FROM rust:1.96-slim-bookworm AS chef
WORKDIR /app
RUN cargo install cargo-chef --locked --version ^0.1
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Work out what the dependencies are, without the source.
FROM chef AS planner
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# Build only the dependencies. This layer is cached until Cargo.toml changes.
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p ghostchase-server

# Now the actual source, which is the only thing that usually changed.
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo build --release -p ghostchase-server \
    && strip target/release/ghostchase-server

# The runtime carries the binary and nothing else. No toolchain, no source,
# no renderer: this is a headless dedicated server.
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Never run as root. Nothing here needs it.
RUN useradd --system --uid 10001 --create-home runner
USER runner
WORKDIR /home/runner

COPY --from=builder /app/target/release/ghostchase-server /usr/local/bin/ghostchase-server

# GameFlow assigns the real port at allocation time and the SDK reports it
# through GAMEFLOW_DEFAULT_PORT. This is documentation, not a promise.
EXPOSE 2567/udp

ENTRYPOINT ["/usr/local/bin/ghostchase-server"]
