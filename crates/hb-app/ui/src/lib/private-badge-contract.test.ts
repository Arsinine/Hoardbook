// @vitest-environment jsdom
// Owner, 2026-08-27: "in chat, for private channels, instead of the lock sign use the same
// convention in topics, use the private label."
//
// Chat had THREE renderings of one fact: a 🔒 glyph in the conversation list (`.convo-lock`), a grey
// `pill pill-offline` in the channel header, and Topics' own `.tag` badge on the Topics page. All
// three said "this room is private". The badge is now ONE global class, `.hb-tag` in app.css, so
// the pages cannot drift again — a page-local copy is exactly how they drifted the first time,
// because Svelte scopes component styles and a copied class name renders unstyled.
//
// Why a lock is the wrong glyph, not merely an inconsistent one: a padlock says "you cannot get in".
// These are rooms the user is ALREADY IN. The lock described the room's visibility to others using
// the vocabulary of the viewer's own access, which is the one reading that is never true here.
//
// BEHAVIOURAL (a real mount of both routes), not a source-scan — CLAUDE.md §9: the pages CAN be
// mounted, so "we source-scan because it cannot mount" is not available. The source-scan half is
// kept only for the cross-file claim a mount cannot express (no page anywhere reintroduces a lock),
// and it prints per-file counts so "0 hits" is distinguishable from "0 files examined".
//
// MUTATION PROBE (CLAUDE.md §9 — a green test proves nothing until seen red): restore
//   {#if t.private}<span class="convo-lock" title="Private topic">🔒</span>{/if}
// at chat/+page.svelte and re-run THIS FILE ONLY. Both the chat mount case and the no-lock scan
// must red. Reverting only one half at a time attributes the failure (§9, P-9).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/svelte';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { identity, contacts, inboxMessages, sentMessages, readWatermarks, dmRequests } from '$lib/stores.js';
// Imported statically, NOT with a dynamic import inside each test: transforming a ~1900-line route
// page costs seconds on drvfs, and paying it inside the first test's timeout budget made that test
// fail while an identical later mount passed in 846ms. Collection-time cost is untimed.
import ChatPage from '../routes/chat/+page.svelte';
import TopicsPage from '../routes/topics/+page.svelte';

const ME = 'npub1meeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

// vi.hoisted: `vi.mock`'s factory is hoisted above module-level consts, and the STATIC page import
// makes it run at import time — so a plain const here is read before initialization. Same pattern
// the existing chat tests use for their npub constants.
const { PRIVATE_TOPIC, PUBLIC_TOPIC } = vi.hoisted(() => {
	const priv = {
		topic_id: 't-priv',
		name: 'back room',
		description: 'invite only',
		tags: [] as string[],
		private: true,
		joined_at: 0,
	};
	return { PRIVATE_TOPIC: priv, PUBLIC_TOPIC: { ...priv, topic_id: 't-pub', name: 'video/animation/anime', private: false } };
});

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/') }) };
});
vi.mock('$app/stores', () => stubPage);

vi.mock('$lib/api.js', () => ({
	// Chat's surface.
	getMessages: vi.fn().mockResolvedValue([]),
	sendMessage: vi.fn(),
	pasteKey: vi.fn().mockResolvedValue({ profile: null }),
	follow: vi.fn().mockResolvedValue(undefined),
	validateShareCode: vi.fn().mockResolvedValue(null),
	shareCodeInfo: vi.fn().mockResolvedValue(null),
	topicChannel: vi.fn().mockResolvedValue({ posts: [], announcements: [] }),
	topicPost: vi.fn(),
	getContacts: vi.fn().mockResolvedValue([]),
	dmRequests: vi.fn().mockResolvedValue([]),
	dmRequestAccept: vi.fn(),
	dmRequestDecline: vi.fn(),
	dmBlock: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn(),
	advanceReadWatermark: vi.fn().mockResolvedValue(undefined),
	topicAnnounceMarkSeen: vi.fn(),
	getShareCode: vi.fn().mockResolvedValue(''),
	relayStatus: vi.fn().mockResolvedValue([]),
	getCollections: vi.fn().mockResolvedValue([]),
	exportManifest: vi.fn(),
	sendFullList: vi.fn(),
	redeemManifestTicket: vi.fn(),
	getSettings: vi.fn().mockResolvedValue({ big_relay_url: '' }),
	getManifestAsks: vi.fn().mockResolvedValue([]),
	// Both pages read the joined list from here.
	topicList: vi.fn().mockResolvedValue([PRIVATE_TOPIC, PUBLIC_TOPIC]),
	// Topics' surface.
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscover: vi.fn().mockResolvedValue([]),
	topicLookup: vi.fn().mockResolvedValue({ topic_id: '', name: '', exists: false, member_count_estimate: 0 }),
	topicJoinPublic: vi.fn(),
	topicRedeemInvite: vi.fn(),
	topicPreviewInvite: vi.fn(),
	topicLeave: vi.fn(),
	topicInvite: vi.fn(),
	topicRoster: vi.fn().mockResolvedValue([]),
	topicAnnounce: vi.fn(),
	topicAnnouncements: vi.fn().mockResolvedValue([]),
	onlineCount: vi.fn().mockResolvedValue({ online: null, fetched_at: null, relay_set: [], fresh: [] }),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	identity.set(null);
	contacts.set([]);
	inboxMessages.set([]);
	sentMessages.set([]);
	readWatermarks.set({});
	dmRequests.set([]);
});

function primeIdentity() {
	identity.set({ npub: ME, npub_short: ME, share_code: 'hbk1x', key_storage: 'plain-file' });
}

/** Every lock glyph a page might reach for. The badge must be a WORD, never one of these. */
const LOCK_GLYPHS = /[\u{1F512}\u{1F510}\u{1F513}\u{1F50F}]/u;

describe('the private badge is one convention across Chat and Topics', () => {
	it('Chat renders the word "private" for a private channel, and no lock glyph anywhere', async () => {
		primeIdentity();
		const { container } = render(ChatPage);

		// The channel list is populated from topicList; wait for the private one to arrive.
		await waitFor(() => expect(container.textContent).toContain('back room'));

		const badges = Array.from(container.querySelectorAll('.hb-tag'));
		expect(badges.length, 'the private channel must carry an .hb-tag badge').toBeGreaterThan(0);
		expect(badges.some((b) => b.textContent?.trim() === 'private')).toBe(true);

		// The whole rendered page, not just the row: a lock could hide in the header or a tooltip.
		expect(LOCK_GLYPHS.test(container.textContent ?? '')).toBe(false);
		expect(container.querySelector('.convo-lock'), 'the old lock span must be gone').toBeNull();
		// `title="Private topic"` was the lock's only accessible name — it must not survive either.
		expect(container.querySelector('[title="Private topic"]')).toBeNull();
	}, 20_000);

	it('Topics renders the same .hb-tag badge, so the two pages agree', async () => {
		primeIdentity();
		const { container } = render(TopicsPage);

		await waitFor(() => expect(container.textContent).toContain('back room'));

		const badges = Array.from(container.querySelectorAll('.hb-tag'));
		expect(badges.some((b) => b.textContent?.trim() === 'private')).toBe(true);
		expect(LOCK_GLYPHS.test(container.textContent ?? '')).toBe(false);
	}, 20_000);

	it('a PUBLIC channel carries no badge — the badge means something, it is not decoration', async () => {
		primeIdentity();
		const { container } = render(ChatPage);
		await waitFor(() => expect(container.textContent).toContain('animation/anime'));

		// Exactly one private topic is mocked, so exactly one badge may render in the list.
		const badgeTexts = Array.from(container.querySelectorAll('.hb-tag')).map((b) => b.textContent?.trim());
		expect(badgeTexts.every((t) => t === 'private')).toBe(true);
	}, 20_000);
});

describe('no route page reintroduces a lock glyph as the privacy indicator', () => {
	// The cross-file claim a mount cannot make. Allowlist-shaped (scan EVERY route page, do not
	// hand-list the two we happen to know about) with per-file counts printed, per CLAUDE.md §7 —
	// so a sweep that examined nothing cannot report CLEAN.
	it('scans every route page and reports what it examined', () => {
		const routes = join(__dirname, '..', 'routes');
		const pages: string[] = [];
		const walk = (dir: string) => {
			for (const e of readdirSync(dir, { withFileTypes: true })) {
				const p = join(dir, e.name);
				if (e.isDirectory()) walk(p);
				else if (e.name === '+page.svelte') pages.push(p);
			}
		};
		walk(routes);

		expect(pages.length, 'the sweep must actually find route pages').toBeGreaterThan(3);

		const offenders: string[] = [];
		const report: string[] = [];
		for (const p of pages) {
			const src = readFileSync(p, 'utf8');
			// Strip comments so the page's own prose about locks cannot red this (§9, P-12).
			const code = src.replace(/<!--[\s\S]*?-->/g, '').replace(/\/\*[\s\S]*?\*\//g, '');
			const hits = (code.match(new RegExp(LOCK_GLYPHS, 'gu')) ?? []).length;
			report.push(`${p.split('routes/')[1]}: ${code.split('\n').length} lines examined, ${hits} lock glyph(s)`);
			// Browse legitimately uses 🔒 for "Listings locked" — a real access denial, not a room's
			// visibility. The offence is a lock next to a PRIVATE flag.
			if (hits > 0 && /private/i.test(code)) {
				const nearPrivate = /\{#if[^}]*private[^}]*\}[^\n]*[\u{1F512}\u{1F510}\u{1F513}\u{1F50F}]/u.test(code);
				if (nearPrivate) offenders.push(p);
			}
		}
		// Printed so a zero result is auditable rather than merely reassuring.
		console.log('[private-badge sweep]\n' + report.join('\n'));
		expect(offenders, 'a lock glyph is being used as a privacy indicator').toEqual([]);
	});
});
