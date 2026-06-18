// Relay defaults + URL validation (pure, so the regression below is unit-testable).
//
// Pre-pivot bug class: the settings UI was wired for the *retired custom HTTP relay*
// (`http://…:3000`) — it hard-required `http(s)://` URLs and pinned a dead bootstrap relay. Nostr
// relays speak WebSocket (`ws://` / `wss://`), so a fresh v0.9 install could neither reach a default
// relay nor let the user add a real one. These two exports fix that and are covered by relays.test.ts.

/** Default seed relays a fresh install rides until the user customises their set. Public Nostr
 *  relays — there is no Hoardbook-run SPOF (spec §Relay Model) — chosen from the set the launch
 *  survey (RELAY_DEPLOY.md §2) verified accept the kinds + brand-new npubs + retention, no PoW.
 *  MUST mirror `hb-app/src/net.rs::DEFAULT_RELAYS`. */
export const DEFAULT_RELAYS = ['wss://relay.damus.io', 'wss://nos.lol', 'wss://relay.primal.net'];

export type RelayUrlCheck = { ok: true; url: string } | { ok: false; error: string };

/** Validate a relay URL the user typed. Nostr relays are `ws://` or `wss://` — NOT `http(s)://`
 *  (the retired custom-relay scheme). Returns the normalized URL on success. */
export function validateRelayUrl(raw: string): RelayUrlCheck {
	const url = raw.trim().replace(/\/+$/, '');
	if (!url) return { ok: false, error: 'Enter a relay URL' };
	if (!url.startsWith('ws://') && !url.startsWith('wss://')) {
		return { ok: false, error: 'Relay URL must start with ws:// or wss://' };
	}
	return { ok: true, url };
}
