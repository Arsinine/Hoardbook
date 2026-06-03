# Hoardbook

A peer-to-peer social phonebook for data hoarders. Publish a signed snapshot of your collection, find others with similar interests, and connect directly — no accounts, no central server that owns your data.

> **Status: v0.3.x — core functionality working. Windows and Linux builds available.**

---

## What it does

Hoardbook lets you publish a signed directory tree of your hoard and discover others doing the same. Generate a keypair locally, fill in your profile, publish your collections, and share your Hoardbook ID. Peers add you as a contact, browse your catalog in a two-pane file viewer, and message you directly.

Profile and collection data is served **peer-to-peer via iroh QUIC** — not cached on any relay. The relay is used only for online-status heartbeats and store-and-forward DMs when the recipient is offline.

---

## Features

- **Identity** — Ed25519 keypairs. Your ID is a ~52-character `hb1_…` string derived from your public key with a checksum. No email, no username, no phone number.
- **Collections** — Publish a signed directory tree with metadata: item counts, estimated size, content tags, format info, per-item notes. Contacts get a two-pane file browser.
- **Profile** — Display name, bio, region, contact email, social links (Reddit, Discord, Matrix, Bluesky, GitHub, etc.), languages, and hoarding stats.
- **Direct P2P** — Profile and collection data is fetched directly over iroh (QUIC/NAT traversal). No relay round-trip for browsing.
- **Direct messages** — Delivered over iroh when the recipient is online; relayed as end-to-end encrypted store-and-forward when offline (X25519 DH + XChaCha20-Poly1305, with `{from,to,sent_at}` as AEAD associated data so ciphertext cannot be replayed).
- **Contacts and groups** — Follow peers by Hoardbook ID, assign local tags and groups, see online/offline status.
- **DHT discovery** — Announce content tags to the mainline DHT (BEP 5). Others can search for peers who hoard the same things without needing your ID first.
- **Saved watches** — Persist a tag/content-type search. The app notifies you when a new matching peer is discovered.
- **File transfer** — Request files from a contact's shared collection over a direct iroh connection. Optional speed cap, download slot limit, and contact-only restriction.
- **System tray** — Closing the window minimises to the system tray. The iroh endpoint, heartbeat, and DHT announce keep running. Quit from the tray to terminate.
- **In-app updater** — Checks for updates silently on launch and lets you install from Settings.
- **Export** — Copy a collection listing to clipboard as plain text or a Markdown checklist.

---

## Privacy model

**Relay-only is the default privacy stance.** Your IP address is never exposed to peers unless you opt in to direct iroh connections (the app warns once before enabling this).

In relay-only mode, peers see your `node_addr` only if you have heartbeated recently — and only via the relay's `/v1/peer/:pubkey` response. The relay operator sees DM metadata (sender, recipient, timing) and stores encrypted ciphertext; it cannot read content.

All signed documents are verified before the relay stores or forwards them. The relay cannot forge anything — every envelope is Ed25519-signed by the originating keypair.

Your data is stored as local signed JSON files (`profile.signed.json`, `<slug>.signed.json`, `keypair.bin`). Nothing lives in a central database.

---

## Architecture

Cargo workspace with three crates:

```
hb-core/    — shared types, Ed25519 crypto, JCS canonicalization, signed envelope format
hb-relay/   — HTTP relay server (axum + SQLite via sqlx)
hb-app/     — Tauri 2.x desktop app (Rust backend + SvelteKit UI)
```

### iroh P2P layer

Each running Hoardbook instance binds an iroh QUIC endpoint. Two custom protocols run over it:

| ALPN | Purpose |
|------|---------|
| `/hoardbook/xfer/1` | File transfer — serve files from shared collections |
| `/hoardbook/node/1` | Node server — serve profile + collections; accept direct DMs |

When a peer is online their `node_addr` is available from the relay heartbeat store. The app connects directly, skipping the relay entirely for profile/collection data.

### Relay HTTP API

The relay is a **bootstrap node and DM relay only** — it does not cache profiles or collections.

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/publish` | Store a signed DM envelope (messages only) |
| `POST` | `/v1/heartbeat` | Update online status + iroh `node_addr` (signed `DocType::Heartbeat`) |
| `GET`  | `/v1/peer/:pubkey` | Online status and `node_addr` for a given key |
| `GET`  | `/v1/messages/:pubkey?signed_at&signature` | Fetch DMs — requires a signed timestamp proving key ownership |
| `GET`  | `/v1/health` | Relay health, stored-peer count, and peer relay list |

Rate limit: 30 requests per IP per minute. Message TTL: 30 days. Mailbox cap: 500 messages per recipient. Heartbeat rows never expire.

### Security notes

- **Transport:** the app requires `https://` relay URLs. `http://` is blocked unless `HB_ALLOW_INSECURE_RELAY=1` is set (dev/test only; never set in production).
- **Mailbox auth:** `/v1/messages/:pubkey` requires a query-string `signed_at` + `signature` over `{purpose, public_key, signed_at}`. Third parties cannot enumerate another user's inbox.
- **Key storage:** private keys are DPAPI-encrypted on Windows (`keypair.bin`) and stored as `chmod 600` plaintext JSON on Linux (`keypair.json`).

---

## Building from source

**Prerequisites:** Rust (stable), Node.js 18+, and the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```sh
git clone https://github.com/Arsinine/Hoardbook
cd Hoardbook

# Run the relay (plain HTTP, defaults to :3000)
cargo run --release -p hb-relay

# Desktop app — dev mode (opens a hot-reload window)
cd crates/hb-app/ui && npm install && cd ../../..
cargo tauri dev --manifest-path crates/hb-app/Cargo.toml

# Desktop app — production build
cargo tauri build --manifest-path crates/hb-app/Cargo.toml
```

---

## Self-hosting a relay

### Relay environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `sqlite://hb-relay.db` | SQLite database path |
| `BIND_ADDR` | `0.0.0.0:3000` | TCP address to listen on |
| `PEER_RELAYS` | _(empty)_ | Comma-separated relay URLs advertised in `/v1/health` |
| `TLS_CERT` | _(unset)_ | Path to a PEM certificate file (enables native HTTPS when set alongside `TLS_KEY`) |
| `TLS_KEY` | _(unset)_ | Path to a PEM private key file |
| `HB_ALLOW_INSECURE_RELAY` | `0` | Set to `1` to permit `http://` relay URLs in dev/test builds |

### Option A — Native TLS (built-in)

The relay binary can terminate TLS directly. Provide a PEM certificate and key (e.g. from Let's Encrypt):

```sh
DATABASE_URL=sqlite:///var/lib/hb-relay/relay.db \
BIND_ADDR=0.0.0.0:443 \
TLS_CERT=/etc/letsencrypt/live/relay.example.com/fullchain.pem \
TLS_KEY=/etc/letsencrypt/live/relay.example.com/privkey.pem \
./hb-relay
```

The relay serves HTTPS when both `TLS_CERT` and `TLS_KEY` are set; plain HTTP otherwise.

### Option B — Caddy reverse proxy (recommended)

Caddy handles certificate provisioning automatically. Install Caddy, point your domain at the server, then:

**`/etc/caddy/Caddyfile`**
```
relay.example.com {
    reverse_proxy localhost:3000
}
```

Run the relay on port 3000 (plain HTTP) and Caddy fronts it with HTTPS:

```sh
DATABASE_URL=sqlite:///var/lib/hb-relay/relay.db \
BIND_ADDR=127.0.0.1:3000 \
./hb-relay
```

### systemd unit

```ini
[Unit]
Description=Hoardbook Relay
After=network.target

[Service]
ExecStart=/usr/local/bin/hb-relay
Environment=DATABASE_URL=sqlite:///var/lib/hb-relay/relay.db
Environment=BIND_ADDR=0.0.0.0:3000
WorkingDirectory=/var/lib/hb-relay
Restart=on-failure
User=hb-relay

[Install]
WantedBy=multi-user.target
```

### Connecting the app to your relay

In the app, go to **Settings → Relays**, paste your relay URL (must be `https://`), click **Add**, then **Save**.

---

## Platform support

| Platform | Status |
|----------|--------|
| Windows | Primary target — fully supported |
| Linux | Supported |
| macOS | Planned (Phase 2) |

---

## Roadmap

- [ ] Docker image for relay self-hosting
- [ ] macOS support
- [ ] Relay peering / community relay network
- [ ] Passphrase-protected keystore (Linux hardening)
- [ ] Static HTML collection export

---

## License

MIT
