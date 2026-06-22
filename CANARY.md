# CANARY.md — the VPS canary (live-relay backbone probe)

> **Owner-run.** Like the relay deploy (`RELAY_DEPLOY.md`) and the M5-launch steps, the dev sandbox
> has no SSH creds to the SG/JP VPSes — the harness + unit tests + this runbook are built and tested
> in-repo; the **actual deploy is owner-run**. Copy-paste below.

## What it is

`hb-it --canary` is a headless probe of the **live** relay backbone — the failure class CI's
ephemeral relay can't see: real relay drift, retention/GC, NIP-13 policy in the wild, cross-region
propagation, DM delivery. It runs continuously on the SG **and** JP VPSes (one each), so each box
independently watches the whole backbone.

On each tick, with an **ephemeral throwaway `npub`** and **every event tagged `hb-canary`** (which
the online/userbase counts + discovery exclude — see `HOARDBOOK_SPEC.md` §Userbase metrics):

1. publish teaser + encrypted listing + presence,
2. fetch them back and verify (Schnorr + decrypt),
3. round-trip a NIP-17 DM,
4. assert SG↔JP cross-region reach (an event published in one region is visible to a client reaching
   both — Nostr relays don't replicate to each other, so this is what "cross-region" actually means),
5. emit TAP + a one-line JSON summary, **nonzero exit on any failure** + an `[ALERT]` log line.

Live relays: SG `ws://141.98.199.138:7777` · JP `ws://45.129.8.225:7777`.

## Usage

```sh
# One cycle, exit code 0 (all green) / 1 (any failure) — the systemd oneshot+timer form.
hb-it --canary --relay ws://141.98.199.138:7777 --relay ws://45.129.8.225:7777

# Long-running daemon form: probe every 600 s, log an [ALERT] line on each failure.
hb-it --canary --interval 600 --relay ws://141.98.199.138:7777 --relay ws://45.129.8.225:7777
```

Output is TAP to stdout plus a final JSON line, e.g.
`{"canary":"pass","passed":4,"failed":0,"npub":"npub1…"}`.

## Build the binary on each VPS

```sh
# On the SG box, then repeat verbatim on the JP box.
sudo apt-get update && sudo apt-get install -y build-essential pkg-config
# Rust (if not present):
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

git clone <hoardbook-repo-url> ~/hoardbook && cd ~/hoardbook
# hb-it has no GUI/webkit deps — it builds without the tauri toolchain.
cargo build --release -p hb-it
sudo install -m 0755 target/release/hb-it /usr/local/bin/hb-it
```

## Deploy: systemd oneshot + timer (recommended)

The oneshot service runs one cycle and **exits nonzero on failure**; `OnFailure=` fires an alert.
The timer runs it every 10 minutes.

`/etc/systemd/system/hb-canary.service`:

```ini
[Unit]
Description=Hoardbook relay-backbone canary (one cycle)
After=network-online.target
Wants=network-online.target
OnFailure=hb-canary-alert@%n.service

[Service]
Type=oneshot
ExecStart=/usr/local/bin/hb-it --canary \
  --relay ws://141.98.199.138:7777 \
  --relay ws://45.129.8.225:7777
# Throwaway-npub-per-run + hb-canary-tagged events → no state, no pollution. Run unprivileged.
DynamicUser=yes
```

`/etc/systemd/system/hb-canary.timer`:

```ini
[Unit]
Description=Run the Hoardbook canary every 10 minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=10min
AccuracySec=30s

[Install]
WantedBy=timers.target
```

Optional alert hook — wire `[ALERT]`/nonzero exit to wherever you watch (email, ntfy, a webhook).
`/etc/systemd/system/hb-canary-alert@.service`:

```ini
[Unit]
Description=Alert on Hoardbook canary failure (%i)

[Service]
Type=oneshot
# Replace with your notifier. journalctl shows the failing cycle's TAP + the [ALERT] JSON line.
ExecStart=/bin/sh -c 'journalctl -u hb-canary.service -n 30 --no-pager | mail -s "hb-canary FAILED on $(hostname)" you@example.com'
```

Enable:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now hb-canary.timer
# Trigger one cycle immediately to confirm wiring:
sudo systemctl start hb-canary.service
journalctl -u hb-canary.service -n 40 --no-pager   # see the TAP + JSON summary
```

### Daemon form (alternative)

If you prefer one long-running process per box instead of a timer:

`/etc/systemd/system/hb-canary.service` (replace the oneshot above):

```ini
[Unit]
Description=Hoardbook relay-backbone canary (daemon)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/hb-it --canary --interval 600 \
  --relay ws://141.98.199.138:7777 \
  --relay ws://45.129.8.225:7777
Restart=always
RestartSec=30
DynamicUser=yes
```

The daemon logs an `[ALERT]` line on each failed cycle (grep the journal / ship it to your log sink);
it does **not** exit on a single failure, so a flapping relay doesn't restart-loop.

## Verify it isn't polluting real data

By construction every canary event carries the `hb-canary` `t` tag, which the counts and discovery
exclude. To confirm against your seed relay DB (strfry), the userbase query that **excludes** the
canary is in `RELAY_DEPLOY.md` (§Userbase / online-now metrics). A canary `npub` must never appear in
`COUNT(DISTINCT pubkey)` once the `hb-canary` tag is filtered out, and a `hb-canary` teaser must never
surface in a tag search (CI's `COUNT3` / `CANARY` suites prove this end-to-end).
