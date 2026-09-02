// QURATOR-137 — the auto-approve UI half (commit c61a1a0's owed UI). `deriveManifestFulfil` gained
// an `answered` input carrying the standing-grant fact, a seventh `answered` state, and the card
// renders it informationally with ZERO verbs — a human clicking "Send the full list" for a request
// the auto-approve loop already answered would mint a SECOND ticket for the same request.
//
// Two layers, both pinned here:
//   1. The pure derivation (order: quarantine → re-serve → own-draft resolution → answered).
//   2. The card component MOUNTED (not source-scanned) — the Carrier-4 lesson: two green tests at
//      their own seams proved derivation and copy while production could never reach the state.
//      Mounting the card with a real `answered` state is what proves the arm renders and carries no
//      button. The backend half (`has_standing_grant`, a thin shim) is pinned in
//      `crates/hb-app/src/commands/chat.rs`.
//
// Copy constraints under test: a standing grant is RESOURCE control, never revocation — no copy may
// state or imply that blocking or removing the contact withdraws their read access — and no expiry
// or time-box language, because grants do not expire (owner rulings 2026-08-31 / 2026-09-01).
// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { deriveManifestFulfil, manifestFulfilFor, MANIFEST_ANSWERED_LINE } from './manifest-fulfil.js';
import type { ManifestRequest } from './request-inbox.js';
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

function manifestReq(opts: { slug: string; fingerprintSeen?: string; authorNpub?: string }): ManifestRequest {
	const fp = opts.fingerprintSeen ?? '';
	return {
		slug: opts.slug,
		fingerprintSeen: fp,
		teaserEventId: undefined,
		mascaraPubkey: undefined,
		authorNpub: opts.authorNpub,
	};
}

describe('deriveManifestFulfil — the `answered` input (QURATOR-137)', () => {
	it('a granted own-collection ask over a Public draft → `answered`, no verb on the card', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, {
			quarantined: false,
			answered: true,
		});
		expect(state).toEqual({ kind: 'answered', slug: 'criterion' });
	});

	it('`answered` defaults OFF — an ungranted ask derives exactly the pre-137 states', () => {
		// The input is optional; every existing caller that passes no `answered` must be unaffected.
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, { quarantined: false });
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});

	it('`answered: false` is the same as absent (the caller-supplied fact is honoured literally)', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, {
			quarantined: false,
			answered: false,
		});
		expect(state).toEqual({ kind: 'public', slug: 'criterion', stale: false });
	});

	it('QUARANTINE SHORT-CIRCUITS BEFORE `answered` is judged — a quarantined peer stays quarantined', () => {
		// The ordering rule: Accept comes first, always. Even with the grant fact in hand, the
		// quarantined card keeps its inert quarantine state.
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, {
			quarantined: true,
			answered: true,
		});
		expect(state).toEqual({ kind: 'quarantine', slug: 'criterion' });
	});

	it('a carrier-4 re-serve ask stays `re-serve` even when answered=true — the loop declines those', () => {
		// `auto_approve` step 0 returns None for ANY authorful ask, so the machine never answers a
		// re-serve request. A card claiming "already answered" for one would be the Carrier-4
		// dead-copy defect again: copy production can never make true.
		const drafts = [makeDraft({ slug: 'films', visibility: 'Public' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'films', authorNpub: 'npub1authorA' }), drafts, {
			quarantined: false,
			answered: true,
		});
		expect(state).toEqual({ kind: 're-serve', slug: 'films', authorNpub: 'npub1authorA' });
	});

	it('a granted ask with NO matching draft stays `missing` — the machine cannot answer what it cannot build', () => {
		// Same honesty rule as re-serve: `send_full_list_inner` resolves the slug against drafts
		// before anything is promised, so `answered` never claims an answer for a missing collection.
		const state = deriveManifestFulfil(manifestReq({ slug: 'renamed-away' }), [makeDraft({ slug: 'other' })], {
			quarantined: false,
			answered: true,
		});
		expect(state).toEqual({ kind: 'missing', slug: 'renamed-away' });
	});

	it('a granted ask over a Private draft stays `private` — build_slug_manifest refuses those by construction', () => {
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Private' })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, {
			quarantined: false,
			answered: true,
		});
		expect(state).toEqual({ kind: 'private', slug: 'criterion' });
	});

	it('a granted ask over an empty draft stays `empty` — the other never-answerable shape', () => {
		const drafts = [makeDraft({ slug: 'criterion', content_types: [] })];
		const state = deriveManifestFulfil(manifestReq({ slug: 'criterion' }), drafts, {
			quarantined: false,
			answered: true,
		});
		expect(state).toEqual({ kind: 'empty', slug: 'criterion' });
	});

	it('manifestFulfilFor threads `answered` through parse + derive in one pass', () => {
		const content = JSON.stringify({ hb: 'manifest_request', slug: 'criterion', fingerprint_seen: 'fp1' });
		const drafts = [makeDraft({ slug: 'criterion', visibility: 'Public' })];
		const out = manifestFulfilFor(content, drafts, { quarantined: false, answered: true });
		expect(out?.state).toEqual({ kind: 'answered', slug: 'criterion' });
	});
});

describe('MANIFEST_ANSWERED_LINE — binding copy constraints (owner rulings 2026-08-31 / 2026-09-01)', () => {
	it('says the request was answered automatically because the peer is already approved', () => {
		expect(MANIFEST_ANSWERED_LINE).toMatch(/answered/i);
		expect(MANIFEST_ANSWERED_LINE).toMatch(/approv/i);
		expect(MANIFEST_ANSWERED_LINE).not.toMatch(/click here|button|send the full list/i);
	});

	it('never states or implies that blocking/removing the contact withdraws read access', () => {
		// A standing grant is RESOURCE control, not revocation. The line is about what the machine
		// already did, never about what withdrawing the grant would do.
		expect(MANIFEST_ANSWERED_LINE).not.toMatch(/block|remove|revoke|withdraw|no longer|stops?/i);
	});

	it('carries no expiry or time-box language — grants do not expire', () => {
		expect(MANIFEST_ANSWERED_LINE).not.toMatch(/expir|time.?box|until|valid for|period|window|hour|day|week|month/i);
	});

	it('names the LIST as the thing that crosses — never the files (MAS-INV-5 + INV-4)', () => {
		expect(MANIFEST_ANSWERED_LINE).toMatch(/\blist\b/i);
		expect(MANIFEST_ANSWERED_LINE).not.toMatch(/\bfiles?\b/i);
		expect(MANIFEST_ANSWERED_LINE).not.toMatch(/download/i);
	});
});

// ── The card, MOUNTED — the seam the Carrier-4 defect slipped through ────────────────────────
// jsdom mounts are the repo's proven pattern (ShareCodeCard.test.ts). This is the half the
// dead-code incident taught: derivation tests and copy tests were each green at their own seam
// while the page never reached the state. Mounting the component with a real `answered` state
// proves the arm exists in the RENDER, not just in the union type.
import { render, cleanup } from '@testing-library/svelte';
import ManifestFulfilCard from './components/ManifestFulfilCard.svelte';

describe('ManifestFulfilCard — the `answered` arm renders informationally, ZERO verbs', () => {
	it('renders the answered line and NO button (no second ticket can be minted from the card)', () => {
		const { container, queryAllByRole } = render(ManifestFulfilCard, {
			props: {
				state: { kind: 'answered', slug: 'criterion' },
				fingerprintSeen: 'fp1',
				onsend: () => {},
				onserve: () => {},
				sending: false,
			},
		});
		expect(container.textContent).toContain('Already answered');
		expect(queryAllByRole('button')).toEqual([]);
		expect(container.querySelector('button')).toBeNull();
		// The card's data-state discriminant is what the route's tests key on — pin it so the arm
		// is identifiable in the DOM, not just by its copy.
		expect(container.querySelector('.mf-card')?.getAttribute('data-state')).toBe('answered');
	});

	it('the `public` arm still renders its verb — the answered state did not eat the click path', () => {
		const { getAllByRole } = render(ManifestFulfilCard, {
			props: {
				state: { kind: 'public', slug: 'criterion', stale: false },
				fingerprintSeen: '',
				onsend: () => {},
				onserve: () => {},
				sending: false,
			},
		});
		expect(getAllByRole('button').length).toBeGreaterThan(0);
	});

	it('the answered arm renders no "Send the full list" text anywhere in the card', () => {
		const { container } = render(ManifestFulfilCard, {
			props: {
				state: { kind: 'answered', slug: 'criterion' },
				fingerprintSeen: '',
				onsend: () => {},
				onserve: () => {},
				sending: false,
			},
		});
		expect(container.textContent).not.toContain('Send the full list');
	});
});
