#!/usr/bin/env bash
# Builds the zip that gets uploaded to GameFlow.
#
# The Dockerfile has to sit at the root of the archive: PrepareBuildContext
# looks for it there and, when it finds one, uses it instead of any engine
# template. That is the whole reason a Rust server works without a platform
# change.
set -euo pipefail

cd "$(dirname "$0")/.."

OUT="${1:-pacman-server.zip}"

if [[ ! -f Dockerfile ]]; then
    echo "no Dockerfile at the repo root, the build would fall back to an engine template" >&2
    exit 1
fi

rm -f "$OUT"

# Cargo.lock is included on purpose: the image should build the versions that
# were tested, not whatever resolves on the day.
#
# `*.env` is excluded so a developer's real .env (which holds the API key) never
# rides along into the build context. The .env.example template is kept.
zip -q -r "$OUT" \
    Dockerfile \
    Cargo.toml \
    Cargo.lock \
    rust-toolchain.toml \
    crates \
    -x '*/target/*' \
    -x '*.zip' \
    -x '*/.git/*' \
    -x '*.env'

# Safety net: refuse to ship a real .env even if the exclude above ever breaks.
# The server image must never carry the backend's secrets.
if unzip -l "$OUT" | grep -Eq '/\.env$'; then
    echo "ERROR: a .env slipped into $OUT — refusing to ship secrets" >&2
    rm -f "$OUT"
    exit 1
fi

echo "wrote $OUT ($(du -h "$OUT" | cut -f1))"
echo
echo "next: upload it in the dashboard, then confirm the build landed:"
echo "  GET /v1/images/builds?game_id=<id>   -> status success, isCurrent true"
