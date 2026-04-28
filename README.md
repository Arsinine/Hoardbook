# Hoardbook

A P2P social phonebook for data hoarders. Share what you have, discover what others are sitting on, and connect with people who actually understand why you have twelve copies of a 4K encode of a 1954 film.

> **Status: early development / pre-alpha.** Core functionality is working. No downloadable releases yet.

---

## What it does

Hoardbook gives data hoarders a way to publish a directory of their collections and find other hoarders with similar interests. You generate a keypair locally, publish a signed snapshot of your collection, and share your Hoardbook ID with people you want to connect with. They add you as a contact, browse your collection, see when you're online, and message you.

No accounts. No servers you have to trust. No IP address exposure by default.

### Features

- **Identity** — Ed25519 keypairs. Your ID is a ~52-character `hb1_…` string. No email, no username, no phone number required.
- **Collections** — Publish a signed directory tree of your hoard with metadata: item counts, estimated size, content tags, format info. Contacts get a two-pane file browser.
- **Profile** — Display name, bio, region, email, social links (Reddit, Discord, Matrix, Bluesky, GitHub, etc.), languages, hoarding stats.
- **Contacts** — Add peers by Hoardbook ID or discover them via the relay's public directory. See online status, filter by local tags you assign.
- **Recommended peers** — Opt in to appear in the relay's public directory so others can find you without needing your ID first.
- **General channel** — Relay-hosted public message board. Post publicly to all peers connected to the same relay.
- **Direct messages** — Encrypted peer-to-peer messages routed through the relay (X25519 DH + ChaCha20-Poly1305). Can be restricted to contacts-only.
- **File transfer** — Request files directly from a contact's shared collection over an iroh P2P connection. Optional speed cap, download slot limit, contact-only restriction.
- **Key succession** — Rotate your key and publish a signed succession document. Your contacts' apps migrate to your new key silently.

---

## Privacy model

Relay-first is the **default**. Your IP address is never exposed to peers unless you explicitly opt in to direct iroh connections (the app warns before doing so).

The relay is a neutral pipe — it stores signed documents and routes messages but cannot read E2E-encrypted content and has no ability to forge documents (all envelopes are Ed25519-signed and verified before storage).

Your data lives in local signed JSON files. There is no central database that owns your identity.

---

## Architecture

Cargo workspace with three crates:

```
hb-core/    — shared types, Ed25519 crypto, signed envelope format
hb-relay/   — HTTP relay server (axum + sqlx + SQLite)
hb-app/     — Tauri 2.x desktop app (Rust backend + SvelteKit UI)
```

### Relay HTTP API

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/publish` | Publish a signed profile, collection, succession, or DM |
| `POST` | `/v1/heartbeat` | Update online status; opt into public directory with `listed: true` |
| `GET`  | `/v1/peer/:pubkey` | Fetch a peer's profile, collections, and online status |
| `GET`  | `/v1/messages/:pubkey` | Fetch DMs addressed to a pubkey |
| `GET`  | `/v1/directory` | List peers that opted into the public directory (require published profile) |
| `GET`  | `/v1/channel/:channel` | Fetch recent public channel messages |
| `POST` | `/v1/channel/:channel` | Post a signed message to a public channel |
| `GET`  | `/v1/name/:display_name` | Check if a display name is taken (anti-spoofing) |
| `GET`  | `/v1/health` | Relay health and stored-document count |

All documents are `SignedEnvelope` — a JCS-canonicalized JSON payload with an Ed25519 signature. The relay verifies every signature before storing anything.

---

## Building from source

**Prerequisites:** Rust (stable), Node.js 18+, and the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```sh
git clone https://github.com/fluxtheory/hoardbook
cd hoardbook

# Build and run the relay (defaults to :3000)
cargo run --release -p hb-relay

# Build the desktop app (dev mode)
cd crates/hb-app/ui && npm install && cd ../../..
cargo tauri dev --manifest-path crates/hb-app/Cargo.toml

# Production build
cd crates/hb-app/ui && npm run build && cd ../../..
cargo tauri build --manifest-path crates/hb-app/Cargo.toml
```

A Docker image for the relay will be published alongside the first release.

---

## Platform support

- **Windows** — primary target
- **Linux** — supported
- **macOS** — Phase 2

---

## Self-hosting a relay

The relay is intentionally simple to self-host. Set `DATABASE_URL` (SQLite path) and `BIND_ADDR`, then run:

```sh
DATABASE_URL=sqlite:///var/lib/hb-relay/relay.db BIND_ADDR=0.0.0.0:3000 ./hb-relay
```

A `docker-compose.yml` will ship with the first release.

Relay operators control their own terms of service. The relay code enforces rate limits (30 req/min per IP), size caps (5 MB per collection), and message caps (500 per inbox), but takes no position on content.

---

## Roadmap

- [ ] Collection snapshot export (plain text + markdown checklist)
- [ ] Docker image for relay self-hosting
- [ ] Relay peering / discovery
- [ ] macOS support
- [ ] Downloadable release builds

---

## License

MIT
