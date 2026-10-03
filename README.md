# Tessel coordinator (day-one toolchain check)

Rust Worker + one coordinator Durable Object per repo, speaking the claim protocol over WebSocket.

## Prerequisites
    rustup target add wasm32-unknown-unknown
    npm i -g wrangler

## Run locally
    npx wrangler dev
    # in another terminal (install websocat, e.g. `brew install websocat`):
    websocat ws://localhost:8787/repo/demo/ws

Paste:

    {"type":"hello","agent":"agent-1","base":"abc123"}

Expect:

    {"type":"welcome","head":"abc123","lease_ms":30000}

Then try `{"type":"heartbeat"}` (expect a not-implemented error) and `garbage` (expect `malformed`).

## Deploy
    npx wrangler deploy
    websocat wss://tessel-coordinator.<your-subdomain>.workers.dev/repo/demo/ws

## Tests
    cargo test
