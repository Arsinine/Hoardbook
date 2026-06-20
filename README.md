# Hoardbook

A peer-to-peer phonebook for data hoarders — the directory layer of the Qurator family. Publish an encrypted snapshot of your collection, let likeminded people find and verify you, and reach them directly. No accounts, and no central server that owns your data.

> **Shipped build: v0.9.6 (Nostr-native, moves no files).** The Nostr pivot landed and the v0.9.6
> threat-tier re-scope is complete: identity is a Nostr `npub`, all signaling/discovery/presence/DMs ride
> the public Nostr relay network, and collection listings are encrypted events. The legacy self-run relay,
> mainline DHT, PEX, and Ed25519 `hb1_` identity are **deleted** — and so is the **in-app file-transfer
> plane**: Hoardbook now **moves no files**. The download/sync role split into a separate companion app,
> **Mascara** ([`MASCARA_SPEC.md`](MASCARA_SPEC.md)); Hoardbook ends at *introduction + encrypted listing*.
> Spec: [`HOARDBOOK_SPEC.md`](HOARDBOOK_SPEC.md) (v0.9.6 — now aligned with the shipped code).

---

## What it does

Generate a Nostr key locally, write a short profile, publish your collections, and hand out your share
code. People add you as a contact, browse your catalog in a two-pane file viewer, and message you
directly. Everything rides public Nostr relays — discovery, presence, your (encrypted) listings, and
DMs — so there is no Hoardbook-run server you depend on, and browsing a peer never connects you to them
directly (it's a relay read + a local decrypt), so it doesn't expose your IP.

Hoardbook is the **phonebook** to Qurator's "club": standalone-capable, but companion-shaped. It finds
and verifies people and shows what they have; the rich social surface (rooms, reputation, enrichment)
lives in Qurator.

---

## Features (v0.9.6)

- **Identity — Nostr `npub` (secp256k1 / BIP-340 Schnorr).** One identity across Hoardbook, Qurator, and
  the wider Nostr network. No email, username, or phone number. (A separate per-app `npub` is available
  if you want Hoardbook kept apart.)
- **Share codes.** Your `npub` is the public part (post it anywhere so people can find and add you). Your
  **share code** — bech32 `hbk1…`, carrying `npub` + a 32-byte **browse-key** — goes only to people you
  want browsing your listings, never into a public thread. A bare `npub1…` follows you and shows your
  public teaser only.
- **Encrypted collections.** Publish a signed directory tree (item counts, sizes, content types, tags,
  format info, per-item notes). The listing payload is **NIP-44-encrypted under your browse-key** and
  published as a Nostr event, so the open web sees only ciphertext; share-code holders decrypt and browse
  it in a two-pane file viewer — **even while you're offline** (the event lives on relays).
- **IP-private browsing.** Browsing is a relay read + local decrypt — no direct connection to the peer,
  so peers never learn your IP. Relays see your `npub` and connection IP like any server (Tor/VPN
  documented for those who want to hide even that).
- **Discovery over Nostr.** Opt-in public teaser carries interest `t`-tags; tag search is a relay filter
  with client-side signature verification. Relay sets are advertised via NIP-65; a curated public-relay
  seed list bootstraps first contact.
- **Direct messages.** NIP-17 gift-wrapped (NIP-44 encryption, per-message ephemeral sender key) — the
  relay can't see the real sender or read content.
- **Impersonation resistance.** Petnames (your local name bound to a key), an always-on word+color
  fingerprint beside every name, and same-name collision alerts — the app never asks you to eyeball a raw
  key.
- **Contacts, groups, and saved watches.** Follow peers, organize them into local groups, and persist
  tag searches that notify you when a new matching peer appears.
- **File transfer — not in Hoardbook (it's the Mascara companion).** Hoardbook **moves no files**: it
  ends at *"here's who has it and how to reach them."* The actual download/sync — a direct iroh (QUIC)
  transfer with an `npub`-signed binding-token gate, integrity check, and follower-gate — lives in the
  separate **Mascara** app ([`MASCARA_SPEC.md`](MASCARA_SPEC.md)), under its own identity, so the only
  IP-exposing direct-P2P moment is never Hoardbook's.
- **Portable backup.** Whole-`~/.hoardbook` backup, passphrase-encrypted (Argon2id → XChaCha20-Poly1305)
  so it restores across machines (the at-rest DPAPI/0600 encryption is not portable). A plaintext export
  exists behind a blunt warning.
- **System tray + updater.** Closing the window minimizes to tray (relay connections + presence keep
  running). The updater is minisign-verified and applies on next restart (Obsidian-style), with a
  "now on vX.Y" notice after.
- **Export.** Copy a collection listing to clipboard as plain text or a Markdown checklist.

---

## Privacy model (in brief)

- **Browsing never exposes your IP to a peer** — it's a relay read. **Hoardbook moves no files**, so
  there is no in-app transfer moment to expose an IP at all; the one direct-P2P, IP-exposing action
  (the actual download) lives in the separate **Mascara** companion, which surfaces its own consent.
- **Listings are encrypted to your share-code holders.** The browse-key is never broadcast — your `npub`
  is what's public. A relay stores only ciphertext for listings and DMs.
- **No single-operator chokepoint.** The default is a spread of public relays (`relay.damus.io`,
  `nos.lol`, `relay.primal.net`); no relay is authoritative, and any Hoardbook-seeded relay is one of
  several, never required.
- **Pseudonymous, not anonymous.** A relay sees `(npub, IP)`; the defense is spread + optional Tor + the
  per-target cost of bridging an IP to a person — not a promise of invisibility. Keep a separate `npub`
  (and keep your real handle out of the public teaser) if you want stronger separation.
- **No recovery.** A lost `npub` private key is a lost identity — back up `~/.hoardbook` before you have a
  problem. Continuity after key loss is social (re-announce a new key under the same external handle).

Full threat model, the v0.9.5 security findings, and the v0.9.6 re-scope rationale are in
[`HOARDBOOK_SPEC.md`](HOARDBOOK_SPEC.md).

---

## Architecture

Cargo workspace, five crates (the old `hb-relay` HTTP server is deleted — Hoardbook runs no relay):

```
hb-core/   — Nostr identity + event types; NIP-44 listing/DM encryption; the `hbk` share-code codec;
             the npub presence-freshness binding; portable Argon2id backup; impersonation fingerprint
hb-net/    — Nostr relay client: multi-relay publish/fetch + dedup-by-id, NIP-65 relay-list resolution,
             NIP-13 PoW, NIP-17 gift-wrapped DMs, and the browse orchestration (publish/fetch/decrypt)
hb-app/    — Tauri 2.x desktop app (Rust backend + SvelteKit UI); drives the Nostr relays (no file transfer)
hb-dpapi/  — Windows at-rest key encryption (DPAPI; cfg(windows))
hb-it/     — L2 integration runner: a headless client over hb-net, run in CI against an ephemeral relay
```

**On the wire:** everything published is a signed Nostr event (NIP-01), via the `nostr` (rust-nostr)
crate. Hoardbook uses a public teaser (parameterized-replaceable), an encrypted collection listing
(parameterized-replaceable, `d`=slug), a presence/online event (replaceable), and NIP-17 gift-wrapped
DMs. Local state is plain files under `~/.hoardbook` (`keys.json`, `collections/`, `contacts/`,
`relays.json`, `groups.json`, `watches.json`, `settings.json`).

**File transfer:** none in Hoardbook. The iroh QUIC data plane, its `npub`-signed binding-token gate,
and the presence address-seal moved to the **Mascara** companion (v0.9.6, INV-4); Hoardbook's presence
event now carries freshness only (no node key, no address). A CI sweep guards the removal.

---

## Direction (v0.9.6)

A threat-tier re-scope (the realistic adversary is a copyright troll / nosy relay operator / casual
scraper — not a nation-state) settled the architecture on:

- **Hoardbook moves no files** *(done in v0.9.6).* Download/sync is a **separate companion app, Mascara**,
  so the thing that *finds* and the thing that *moves* are different apps with different trust boundaries.
  The in-app transfer plane has been removed from Hoardbook; Mascara is its own ongoing effort.
- **Durable encrypted listings + offline browse are kept** (the listing is encrypted and the browse-key
  is never broadcast, so it isn't a world-readable honeypot at this tier).
- **One family `npub` across Hoardbook + Qurator is the default again** (a separate `npub` stays an
  option, not a forced default).
- **Keep the browse-key off public threads; spread across public relays; don't market it as a piracy
  tool.**

---

## Roadmap

**Shipped (v0.9.6):**
- [x] secp256k1 `npub` identity; `hbk` share codes; portable passphrase backup
- [x] Nostr relays for discovery / presence / DMs (NIP-17, NIP-44, NIP-65, NIP-13); public-relay defaults
- [x] Encrypted collection listings; relay-mediated (IP-private) browsing; offline browse
- [x] Retired `hb-relay`, mainline DHT, PEX, UPnP/NAT-PMP, Ed25519 `hb1_` identity
- [x] **Hoardbook moves no files** — removed the in-app iroh file-transfer plane (→ Mascara companion);
      presence is freshness-only; INV-4 CI sweep guards it (v0.9.6)

**Next:**
- [ ] Private collections (per-trusted-`npub` encrypted; Phase 2)
- [ ] M6 polish — fs-watch snapshot auto-update, relay-derived userbase/online count (no telemetry),
      profiling CI gate
- [ ] Hard Nostr kind registration (currently provisional)
- [ ] macOS support; OS-keyring keystore (Linux/macOS hardening)
- [ ] Static-HTML collection export; Qurator integration panel (shared `npub`)

---

## Building from source

**Prerequisites:** Rust (stable), Node.js 18+, and the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```sh
git clone https://github.com/Arsinine/Hoardbook
cd Hoardbook

# Workspace tests (includes the L2 integration runner; CI runs it against an ephemeral relay)
cargo test --workspace

# Desktop app — dev mode (opens a hot-reload window)
cd crates/hb-app/ui && npm install && cd ../../..
cargo tauri dev --manifest-path crates/hb-app/Cargo.toml

# Desktop app — production build
cargo tauri build --manifest-path crates/hb-app/Cargo.toml
```

Hoardbook needs **no relay of its own** — it talks to public Nostr relays out of the box.

---

## Relays

By default the app uses a spread of public Nostr relays (`wss://relay.damus.io`, `wss://nos.lol`,
`wss://relay.primal.net`) and advertises your chosen relays via NIP-65. You can add or replace relays in
**Settings → Relays** (only `wss://` is accepted).

Running your own is optional and uses an **off-the-shelf Nostr relay** (e.g. [strfry](https://github.com/hoytech/strfry)
or [nostr-rs-relay](https://github.com/scsibug/nostr-rs-relay)) — there is no Hoardbook-specific relay
software to maintain. Add its URL to your relay set and advertise it via NIP-65. Operator notes are in
[`RELAY_DEPLOY.md`](RELAY_DEPLOY.md).

---

## Platform support

| Platform | Status |
|----------|--------|
| Windows | Primary target — fully supported |
| Linux | Supported |
| macOS | Planned (Phase 2) |

---

## License

MIT
