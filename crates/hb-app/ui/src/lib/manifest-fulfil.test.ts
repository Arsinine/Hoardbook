// M17 W7.1b — the manifest-request fulfilment card's pure state derivation (table-driven over all
// five states) + the post-export copy invariant sweeps (MAS-INV-5 + INV-4: no "Download"/"Send").
// The card itself is a thin presentational Svelte component (see ManifestFulfilCard.svelte); every
// branch is decided by `deriveManifestFulfil` here, so pinning the helper pins the card.
import { describe, expect, it } from 'vitest';
import {
	deriveManifestFulfil,
	manifestFulfilFor,
	MANIFEST_EXPORTED_TOAST,
	MANIFEST_PRIVATE_LINE,
	MANIFEST_EMPTY_LINE,
	MANIFEST_MISSING_LINE,
	MANIFEST_STALE_NOTE,
	MANIFEST_BIG_RELAY_HINT,
	MANIFEST_BIG_RELAY_LINK,
} from './manifest-fulfil.js';
import { parseManifestRequest, type ManifestRequest } from './request-inbox.js';
import type { Collection } from './types.js';

function makeDraft(opts: Partial<Collection> & { slug: string }): Collection {
	return {
		path_alias: `/alias/${opts.slug}`,
		item_count: 10,
		total_bytes: 1024,
		content_types: ['ebook'],
		tags: [],
		languages: [],
		last_updated: '2026-01-01T00:00:00Z',
		listing: [],
		...opts,
	};
}

function manifestReq(opts: { slug: string; fingerprintSeen?: string }): ManifestRequest {
	const fp = opts.fingerprintSeen ?? '';
	return {
		slug: opts.slug,
		fingerprintSeen: fp,
		teaserEventId: undefined,
		mascaraPubkey: undefined,
	};
}

describe('deriveManifestFulfil — table-driven over all five states', () => {
	it('Public draft → exportable (primary)', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});

	it('Private draft → inert explanatory line, no button', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Private' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state).toEqual({ kind: 'private', slug: 'criterion' });
	});

	it('draft with no content types → inert line pointing at the fix', () => {
		const drafts = [makeDraft({ slug: 'criterion', content_types: [] })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state).toEqual({ kind: 'empty', slug: 'criterion' });
	});

	it('no matching draft (renamed / deleted / never yours) → inert "do not have it any more"', () => {
		const drafts = [makeDraft({ slug: 'something-else' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state).toEqual({ kind: 'missing', slug: 'criterion' });
	});

	it('Q7 request inbox → quarantine, ZERO action buttons (Accept first, always)', () => {
		// Even when the slug matches a Public draft, quarantine short-circuits to the inert state.
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: true });
		expect(state).toEqual({ kind: 'quarantine', slug: 'criterion' });
	});
});

describe('deriveManifestFulfil — absent visibility ⇒ Public (pre-M10 drafts)', () => {
	it('a draft with visibility===undefined is treated as Public, not refused', () => {
		// Pre-M10 collections have no `visibility` field. The card must not mis-render them as Private
		// (the spec pins this: "handle the absent case as Public or you will mis-render pre-M10 drafts").
		const drafts = [makeDraft({ slug: 'criterion', visibility: undefined })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state.kind).toBe('public');
	});
});

describe('deriveManifestFulfil — fingerprint staleness is information, not a gate', () => {
	it('a fresh fingerprint (matches the snapshot) → not stale, no note', () => {
		const drafts = [makeDraft({ slug: 'criterion', snapshot_fingerprint: 'fp-current' })];
		const state = deriveManifestFulfil(
			manifestReq({ slug: 'criterion', fingerprintSeen: 'fp-current' }),
			drafts,
			{ quarantined: false },
		);
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});

	it('a stale fingerprint (differs from snapshot) → STILL exportable, stale=true → note shows', () => {
		// The request carries an older fingerprint_seen; the current snapshot differs. The export
		// still fires (a fresh manifest is what they want); the card surfaces a "stale" note.
		const drafts = [makeDraft({ slug: 'criterion', snapshot_fingerprint: 'fp-new' })];
		const state = deriveManifestFulfil(
			manifestReq({ slug: 'criterion', fingerprintSeen: 'fp-old' }),
			drafts,
			{ quarantined: false },
		);
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: true });
	});

	it('an absent snapshot_fingerprint on the draft → never stale (no basis to compare)', () => {
		const drafts = [makeDraft({ slug: 'criterion' /* no snapshot_fingerprint */ })];
		const state = deriveManifestFulfil(
			manifestReq({ slug: 'criterion', fingerprintSeen: 'whatever' }),
			drafts,
			{ quarantined: false },
		);
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});

	it('an empty fingerprintSeen on the request → never stale (the requester sent no fingerprint)', () => {
		const drafts = [makeDraft({ slug: 'criterion', snapshot_fingerprint: 'fp-current' })];
		const state = deriveManifestFulfil(
			manifestReq({ slug: 'criterion', fingerprintSeen: '' }),
			drafts,
			{ quarantined: false },
		);
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});
});

describe('manifestFulfilFor — wraps parse + derive (one call from the route)', () => {
	it('returns null for an ordinary chat message (no manifest_request tag)', () => {
		expect(manifestFulfilFor('hi there', [], { quarantined: false })).toBeNull();
		expect(manifestFulfilFor('{"hb":"something_else","slug":"x"}', [], { quarantined: false })).toBeNull();
	});

	it('parses a manifest_request JSON and derives the state in one pass', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const content = JSON.stringify({
			hb: 'manifest_request',
			slug: 'criterion',
			fingerprint_seen: 'fp-1',
		});
		const out = manifestFulfilFor(content, drafts, { quarantined: false });
		expect(out).not.toBeNull();
		expect(out!.request.slug).toBe('criterion');
		expect(out!.request.fingerprintSeen).toBe('fp-1');
		expect(out!.state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});

	it('quarantine flag propagates into the state', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const content = JSON.stringify({ hb: 'manifest_request', slug: 'criterion' });
		const out = manifestFulfilFor(content, drafts, { quarantined: true });
		expect(out!.state.kind).toBe('quarantine');
	});
});

describe('MAS-INV-5 + INV-4 — no "Download" anywhere; no Hoardbook-side "Send" affordance', () => {
	// The spec mandates the post-export copy: *"Saved {filename}. Hoardbook moves no files — send it
	// to them yourself."* That is the HONEST last-mile note: Hoardbook writes the file (INV-4 — moves
	// no bytes) and the owner is the courier. "No 'Download' / no 'Send'" (MAS-INV-5) means NO
	// Hoardbook-side transfer affordance — a Download button, a Send button, an auto-deliver verb.
	// The word "send" inside the courier instruction is the OPPOSITE: it tells the owner Hoardbook
	// did NOT send, so they must. These sweeps pin the distinction.
	it('MANIFEST_EXPORTED_TOAST never offers "Download" (Hoardbook-side transfer)', () => {
		const toast = MANIFEST_EXPORTED_TOAST('criterion.hbmanifest');
		expect(toast).toMatch(/saved/i);
		expect(toast).toMatch(/moves no files/i);
		expect(toast.toLowerCase()).not.toMatch(/\bdownload\b/);
	});

	it('MANIFEST_EXPORTED_TOAST carries the honest last-mile instruction (you send it yourself)', () => {
		// The "send it to them yourself" line is the spec-mandated instruction that Hoardbook did
		// NOT deliver — the owner is the courier. This is positive copy, not a forbidden affordance.
		const toast = MANIFEST_EXPORTED_TOAST('criterion.hbmanifest').toLowerCase();
		expect(toast).toMatch(/send it to them yourself/);
		expect(toast).toMatch(/hoardbook moves no files/);
	});

	it('all inert-state copy is honest — no "Download", no Hoardbook-side "Send"', () => {
		// The inert lines (Private / empty / missing / stale / big-relay) never offer a transfer
		// surface. "send" appears ONLY in the post-export toast's courier instruction (tested above).
		const lines = [
			MANIFEST_PRIVATE_LINE,
			MANIFEST_EMPTY_LINE,
			MANIFEST_MISSING_LINE('criterion'),
			MANIFEST_STALE_NOTE,
			MANIFEST_BIG_RELAY_HINT,
			MANIFEST_BIG_RELAY_LINK,
		];
		for (const line of lines) {
			expect(line.toLowerCase()).not.toMatch(/\bdownload\b/);
			expect(line.toLowerCase()).not.toMatch(/\bsend\b/);
		}
	});

	it('the big-relay hint is honest about who moves the bytes', () => {
		// The big-relay path is the owner publishing the manifest themselves to their own big relay;
		// the browser fetches it. Hoardbook still doesn't deliver to the browser — it just hosts the
		// file at a URL the browser can pull.
		expect(MANIFEST_BIG_RELAY_HINT.toLowerCase()).not.toMatch(/\bdownload\b/);
		expect(MANIFEST_BIG_RELAY_HINT.toLowerCase()).not.toMatch(/\bsend\b/);
	});
});

describe('deriveManifestFulfil — the Public case is the EXACT existing export path (slug passthrough)', () => {
	// The card never re-implements export. It calls onexport(slug) and the route's handler runs the
	// same handleExport(slug,'manifest') save dialog → exportManifest(slug, path) → toast. Pin that
	// the slug in the Public state is the request slug verbatim (not a derived/normalized variant).
	it('the slug on the public state is the request slug, unchanged', () => {
		const drafts = [makeDraft({ slug: 'criterion' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state.kind).toBe('public');
		if (state.kind === 'public') expect(state.slug).toBe('criterion');
	});

	it('slug matching is case-sensitive (matches the backend, which keys drafts by exact slug)', () => {
		const drafts = [makeDraft({ slug: 'criterion' })];
		// A differently-cased slug does NOT match — the owner has no draft "Criterion".
		const state = deriveManifestFulfil(manifestReq({ slug: 'Criterion' }), drafts, { quarantined: false });
		expect(state.kind).toBe('missing');
	});
});
