# Hoardbook

**A peer-to-peer phonebook for data hoarders.** Find people who collect what you collect, see what they have, and reach them directly — with no account to sign up for and no company server in the middle.

Hoardbook is a small desktop app. You write a short profile, publish a catalog of your collections, and hand your share code to the people you want browsing it. They add you as a contact, page through your catalog in a familiar two-pane file viewer, and message you directly. Everything runs over the public [Nostr](https://nostr.com) network, so there's no Hoardbook server that can go down, get seized, or quietly own your data.

> **Current release: v0.12.5.** Hoardbook handles *introductions and catalogs only*. It moves no files and has no download feature — once you've found each other, you arrange the transfer however you already do. Your identity is an ordinary Nostr key, so the same key works anywhere else on the network.

---

## Features

- **No accounts, no central server.** Your identity is a key you generate on your own machine. There's no email, username, phone number, or signup — and nothing for an operator to lock you out of or hand over.
- **You decide who sees your catalog.** Your collections are published encrypted. A bare public key lets people *follow* you and see a teaser; your **share code** is what unlocks the full catalog — give it only to people you trust.
- **Browsing keeps your location private.** Looking at someone's catalog reads from a relay and decrypts locally — it never connects you straight to them, so peers don't learn your IP.
- **Find people by what they collect.** Tag your collections by interest and search across the network for others who match. Save a search and get notified when a new matching person shows up.
- **Direct, private messages.** Message any contact end-to-end encrypted. Relays can't read your messages or even tell who sent them.
- **Built-in defense against impersonation.** Every contact gets a local nickname and an at-a-glance word-and-color fingerprint, and Hoardbook warns you when a stranger shows up wearing a contact's name. You never have to eyeball a raw key.
- **Stays current on its own.** Publish a collection and Hoardbook re-publishes the catalog automatically when the underlying folder changes.
- **Portable, encrypted backups.** Back up your whole identity behind a passphrase and restore it on another machine. Lose the key without a backup and the identity is gone — so back up early.
- **Lives in the tray and updates itself.** Closing the window keeps you online in the background; updates are signature-verified and applied on the next restart.

---

## How it works

1. **Generate your key.** Hoardbook creates your identity locally on first launch — no signup, no server.
2. **Add your collections.** Point Hoardbook at the folders you want to share. It publishes an encrypted catalog (folder tree, item counts, sizes, tags, notes) — never the files themselves.
3. **Hand out your share code.** Post your public key anywhere so people can follow you; give your share code to the people you actually want browsing your catalog.
4. **Discover and connect.** Search by interest tags, browse catalogs, add contacts, and message them directly.
5. **Arrange the transfer yourselves.** Hoardbook ends at *"here's who has it and how to reach them."* It moves no files and offers no download — when you want something, you and the other person work it out directly, with whatever you already use.

---

## Privacy

Hoardbook is **pseudonymous, not anonymous**, and it's honest about the line:

- **Catalogs are encrypted to your share-code holders** — the open network only ever sees ciphertext.
- **Browsing never exposes your IP to a peer** — it's a relay read plus a local decrypt.
- **No single point of control** — Hoardbook uses a spread of public relays by default, and no one of them is authoritative.
- **What a relay can still see** is your public key and your connection's IP, the same as any server. If you want to hide even that, route through Tor or a VPN, and keep a separate key for Hoardbook.

---

## Install

Prebuilt Windows binaries are published with each release. Linux is supported from source; macOS is planned.

| Platform | Status |
|----------|--------|
| Windows  | Primary target — fully supported |
| Linux    | Supported |
| macOS    | Planned |

## Building from source

**Prerequisites:** Rust (stable), Node.js 18+, and the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```sh
git clone https://github.com/Arsinine/Hoardbook
cd Hoardbook

# Run the test suite
cargo test --workspace

# Desktop app — dev mode (hot-reload window)
cd crates/hb-app/ui && npm install && cd ../../..
cargo tauri dev --manifest-path crates/hb-app/Cargo.toml

# Desktop app — production build
cargo tauri build --manifest-path crates/hb-app/Cargo.toml
```

Hoardbook talks to public Nostr relays out of the box — there's no relay of its own to run.

---

## Relays

By default Hoardbook spreads across public Nostr relays (`nos.lol`, `relay.primal.net`). You can add or swap relays in **Settings → Relays** (only `wss://` is accepted).

Want to run your own? Use any off-the-shelf Nostr relay — [strfry](https://github.com/hoytech/strfry) or [nostr-rs-relay](https://github.com/scsibug/nostr-rs-relay) — add its URL to your relay set, and you're done. There's no Hoardbook-specific relay software to maintain.

**Before you change your relay set, know what it does and doesn't buy you:**

- **What you gain is metadata privacy, not secrecy.** Your catalog and messages are encrypted before they reach *any* relay, so no operator has ever been able to read them. What a relay does see is your public key, your IP, and which queries you run — and on your own relay, that's seen by you instead of by a stranger.
- **Your own relay doesn't make you anonymous.** It still sees your key and your IP, because you're still connecting to it. If you want that hidden, route through Tor or a VPN.
- **Publishing is not recallable.** Anything already sent to the public defaults stays on them regardless of what you switch to later.
- **Replacing the defaults makes you undiscoverable to everyone still on them.** Hoardbook uses your configured relay set exactly as you set it — so if you drop the public relays and keep only your own, you can only be found by people who have also added it. Adding is safe; replacing is the move that costs you reach.

Settings also has an optional **big relay** — a higher-capacity relay you run for collections too large to publish whole. It's your infrastructure: if it's down or unreachable, people holding your share code fall back to the preview listing until it returns.

## License

Released under the MIT License.
