#!/usr/bin/env bash
#
# Build and start the Palladio architecture plugin service.
#
#   ./run.sh                    build (release), then serve on port 3022
#   ./run.sh --no-build         start the last build
#   PORT=4000 ./run.sh          serve on another port
#   SIM_THREADS=16 ./run.sh     simulation threads (default: one per CPU)
#
# The service verifies session tokens against the backend, which it finds through
# BACKEND_API_URL (default http://localhost:8080/api, the dev stack of
# mdeo-cloud/tools/run-dev.sh). Register it as a plugin under http://localhost:$PORT/ and
# enable it in a project. All settings: README.md.
#
set -Eeuo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export PORT="${PORT:-3022}"
export BACKEND_API_URL="${BACKEND_API_URL:-http://localhost:8080/api}"

if [[ "${1:-}" != "--no-build" ]]; then
    (cd "$DIR" && cargo build --release --quiet -j "${BUILD_JOBS:-4}")
fi

exec "$DIR/target/release/palladio-arch-service" serve
