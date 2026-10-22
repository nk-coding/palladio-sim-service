# syntax=docker/dockerfile:1
#
# The Palladio Simulation plugin service as a container image.
#
#   docker build -t palladio-sim-service .
#   docker run --rm -p 3022:3022 -e BACKEND_API_URL=http://backend:8080/api palladio-sim-service
#
# SimOxide is fetched at the commit in simoxide.rev (Cargo.toml names it as the path dependency
# ../simoxide). The runtime is glibc (Debian slim), not musl: the simulator allocates heavily and
# glibc's malloc with its default arenas is what the throughput measurements used.
#
# Never build with the cargo feature `exact`: it links code without a licence grant (README).

ARG RUST_IMAGE=rust:1.98.0-slim-trixie
ARG RUNTIME_IMAGE=debian:trixie-slim

FROM ${RUST_IMAGE} AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src

# SimOxide first, so its checkout stays cached while only this repository changes.
COPY simoxide.rev /src/simoxide.rev
RUN rev="$(tr -d '[:space:]' < /src/simoxide.rev)" \
    && git init -q simoxide \
    && git -C simoxide fetch -q --depth 1 https://github.com/nk-coding/simoxide.git "$rev" \
    && git -C simoxide checkout -q --detach FETCH_HEAD \
    && test "$(git -C simoxide rev-parse HEAD)" = "$rev"

COPY . /src/palladio-sim-service
WORKDIR /src/palladio-sim-service

# Comma-separated cargo features, e.g. "mimalloc". "exact" is refused.
ARG CARGO_FEATURES=""
# Parallel compile jobs (empty: one per CPU).
ARG BUILD_JOBS=""
RUN case ",${CARGO_FEATURES}," in *,exact,*) echo "the feature 'exact' must never be built into an image (licence)" >&2; exit 1;; esac \
    && test "$(tr -d '[:space:]' < simoxide.rev)" = "$(git -C /src/simoxide rev-parse HEAD)"
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/palladio-sim-service/target,sharing=locked \
    cargo build --release --locked --bin palladio-sim-service \
        ${CARGO_FEATURES:+--features "$CARGO_FEATURES"} ${BUILD_JOBS:+-j "$BUILD_JOBS"} \
    && cp target/release/palladio-sim-service /palladio-sim-service

FROM ${RUNTIME_IMAGE}
# curl for the health check only.
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin sim
COPY --from=build /palladio-sim-service /usr/local/bin/palladio-sim-service
USER 10001
ENV PORT=3022 \
    HOST=0.0.0.0 \
    RUST_LOG=info
EXPOSE 3022
# The service shuts down gracefully on SIGINT (Ctrl-C); SIGTERM to PID 1 would be ignored.
STOPSIGNAL SIGINT
HEALTHCHECK --interval=10s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS -o /dev/null "http://127.0.0.1:${PORT}/" || exit 1
ENTRYPOINT ["palladio-sim-service"]
CMD ["serve"]
