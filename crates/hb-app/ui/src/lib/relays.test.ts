import { describe, it, expect } from 'vitest';
import { DEFAULT_RELAYS, effectiveRelays, validateRelayUrl } from './relays';
import defaultRelaysJson from './default_relays.json';

// Regression for the pre-pivot relay-wiring bugs (the settings UI was built for the retired
// http://…:3000 custom relay). Each test FAILS on the buggy code and PASSES on the fix.

describe('validateRelayUrl — accepts ws/wss, not http (the bug)', () => {
	it('accepts wss:// — the old validation REJECTED this, demanding http://', () => {
		expect(validateRelayUrl('wss://relay.damus.io')).toEqual({ ok: true, url: 'wss://relay.damus.io' });
	});

	it('accepts ws:// (e.g. a local/self-hosted relay)', () => {
		expect(validateRelayUrl('ws://127.0.0.1:7777').ok).toBe(true);
	});

	it('rejects http:// and https:// (the retired custom-relay scheme)', () => {
		expect(validateRelayUrl('http://198.51.100.1:3000').ok).toBe(false);
		expect(validateRelayUrl('https://relay.example').ok).toBe(false);
	});

	it('rejects empty / scheme-less input', () => {
		expect(validateRelayUrl('').ok).toBe(false);
		expect(validateRelayUrl('   ').ok).toBe(false);
		expect(validateRelayUrl('relay.damus.io').ok).toBe(false);
	});

	it('trims and strips a trailing slash', () => {
		expect(validateRelayUrl('  wss://nos.lol/  ')).toEqual({ ok: true, url: 'wss://nos.lol' });
	});
});

describe('DEFAULT_RELAYS — real wss seeds, not the dead bootstrap', () => {
	it('is non-empty so a fresh install can reach a relay (the bug: zero relays)', () => {
		expect(DEFAULT_RELAYS.length).toBeGreaterThan(0);
	});

	it('are all wss:// public relays', () => {
		for (const r of DEFAULT_RELAYS) expect(r.startsWith('wss://')).toBe(true);
	});

	it('has at least two DISTINCT relays (INV-5 floor: never collapse to one relay)', () => {
		expect(new Set(DEFAULT_RELAYS).size).toBeGreaterThanOrEqual(2);
	});

	it('is exactly the shared default_relays.json (audit I-2: single source of truth, also parsed by net.rs)', () => {
		expect(DEFAULT_RELAYS).toEqual(defaultRelaysJson);
	});

	it('does NOT include the retired pre-pivot bootstrap relay', () => {
		expect(DEFAULT_RELAYS).not.toContain('http://198.51.100.1:3000');
		for (const r of DEFAULT_RELAYS) {
			expect(r).not.toContain(':3000');
			expect(r.startsWith('http://')).toBe(false);
		}
	});
});

describe('effectiveRelays — the Settings UI shows the backend-configured set verbatim (QURATOR-208)', () => {
	it('returns a non-empty configured set VERBATIM — the backend reconciles defaults at startup, the UI must not diverge from what it dials', () => {
		// The reconciled shape the Rust side persists for the owner's case plus a custom relay:
		// custom first (persisted order kept), then every default. Whatever the backend saved,
		// Settings must display exactly that list — a second, divergent "effective set" computed
		// on the UI side is how "Settings shows 2, backend uses 4" reports happen.
		//
		// MUTATION (P-10): in `effectiveRelays` (relays.ts), change the body to
		// `return [...DEFAULT_RELAYS];` — the UI would ignore the configured set and both asserts
		// red (toEqual(configured) fails, and the set would equal DEFAULT_RELAYS).
		const configured = ['wss://custom.example', ...DEFAULT_RELAYS];
		expect(effectiveRelays(configured)).toEqual(configured);
		expect(effectiveRelays(configured)).not.toEqual(DEFAULT_RELAYS);
	});

	it('falls back to the shared defaults only while nothing is configured (fresh install)', () => {
		// MUTATION (P-10): in `effectiveRelays`, change the body to `return configured;` — an
		// empty configured set would be returned as-is and this assert reds.
		expect(effectiveRelays([])).toEqual(DEFAULT_RELAYS);
	});
});
