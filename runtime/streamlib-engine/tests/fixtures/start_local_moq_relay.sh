#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Build (once) and run Cloudflare's draft-16 moq-relay-ietf on localhost:4443
# with a self-signed certificate. Pinned to moq-rs at the moq-transport-v0.16.2
# tag, the draft the engine's vendored moq-transport speaks.
#
#   start_local_moq_relay.sh            # build if needed, then run in the foreground
#   start_local_moq_relay.sh --build    # build if needed, then exit
#   MOQ_RELAY_DIR=/tmp/x start_local_moq_relay.sh
#
# The engine dials it with STREAMLIB_MESH_MOQ_RELAY_URL=https://localhost:4443/local
# and STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE=1 (dev-only).
set -euo pipefail

MOQ_RS_REV="66f27b87a639ca1b7a28acb46b1c864c2e374ff7"
MOQ_RELAY_DIR="${MOQ_RELAY_DIR:-/tmp/streamlib-moq-relay-ietf}"
PORT="${MOQ_RELAY_PORT:-4443}"

if [ ! -x "$MOQ_RELAY_DIR/target/release/moq-relay-ietf" ]; then
	if [ ! -d "$MOQ_RELAY_DIR/.git" ]; then
		git clone --quiet https://github.com/cloudflare/moq-rs.git "$MOQ_RELAY_DIR"
	fi
	git -C "$MOQ_RELAY_DIR" -c advice.detachedHead=false checkout -q "$MOQ_RS_REV"
	(cd "$MOQ_RELAY_DIR" && cargo build --release --bin moq-relay-ietf)
fi

[ "${1:-}" = "--build" ] && exit 0

CERT_DIR="$MOQ_RELAY_DIR/localhost-cert"
mkdir -p "$CERT_DIR"
if [ ! -f "$CERT_DIR/localhost.crt" ]; then
	openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
		-keyout "$CERT_DIR/localhost.key" -out "$CERT_DIR/localhost.crt" -days 10 \
		-subj "/CN=localhost" \
		-addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1" 2>/dev/null
fi

exec "$MOQ_RELAY_DIR/target/release/moq-relay-ietf" \
	--bind "[::]:$PORT" \
	--tls-cert "$CERT_DIR/localhost.crt" \
	--tls-key "$CERT_DIR/localhost.key"
