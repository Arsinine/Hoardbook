// @vitest-environment jsdom
// QURATOR-338 — "Contacts does not auto-refresh. You need to switch to another page and then
// switch back before it will update contact's online status." (owner, 2026-09-26)
//
// BEHAVIOURAL repro (the q135 mounting pattern, plus fake timers): mount Contacts with only
// `$lib/api.js` mocked, seed one contact, let the MOUNT poll answer (Offline), then advance the
// 20 s poll with the backend answer CHANGED. The pill must flip IN PLACE, without a remount —
// that is what the `onlineData` → `freshSeen` → `presenceOf` → `withPresence` → `presenced` →
// rows chain owes the user while parked on the page. Both directions are pinned: a peer coming
// ONLINE while you watch, and a peer going OFFLINE while you watch.
//
// jsdom limits, stated honestly: this proves the rendered pill follows the poll's ANSWER on a
// tick. It does not prove the real backend's cache changes while parked — that half is Rust
// (online.rs refreshes at most once per its 60 s REFRESH_INTERVAL, and its result is only READ
// by the next poll, so the in-design screen lag is ≤ ~80 s). If these tests are green as
// written, the frontend reactive chain is exonerated and the defect lies behind the mock —
// the Tauri command or the real webview — not in Svelte reactivity.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { onlineCount } from '$lib/api.js';
import { contacts, contactsLoadError } from '$lib/stores.js';
import type { ContactSummary, Profile } from '$lib/types.js';

// The api mock — every Tauri command Contacts imports is stubbed (the q135 pattern, plus
// applyKeyGrants which the page calls at mount).
vi.mock('$lib/api.js', () => ({
	follow: vi.fn().mockResolvedValue(undefined),
	refreshContact: vi.fn().mockResolvedValue(undefined),
	unfollowContact: vi.fn().mockResolvedValue(undefined),
	setContactTags: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	groupsCreate: vi.fn().mockResolvedValue(undefined),
	groupsDelete: vi.fn().mockResolvedValue(undefined),
	groupsAssign: vi.fn().mockResolvedValue(undefined),
	groupsUnassign: vi.fn().mockResolvedValue(undefined),
	groupsCreateWithMembers: vi.fn().mockResolvedValue(undefined),
	contactUpdateGroups: vi.fn().mockResolvedValue(undefined),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	applyKeyGrants: vi.fn().mockResolvedValue(undefined),
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: null, relay_set: [], fresh: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const BASE = 1_700_000_000_000; // fixed wall clock so every Date comparison is deterministic
const NPUB = 'npub1q338' + 'a'.repeat(50);

const PROF: Profile = {
	display_name: 'Q338 Peer',
	tags: [],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: [],
	updated: '2026-08-01T00:00:00Z',
};

function peer(overrides: Partial<ContactSummary>): ContactSummary {
	return {
		npub: NPUB,
		has_browse_key: false,
		collections: [],
		online: false,
		last_fetched: new Date(BASE).toISOString(),
		local_tags: [],
		profile: PROF,
		...overrides,
	};
}

/** An answered presence payload with no beacon for anyone (⇒ pill reads Offline, QURATOR-216). */
function offlineAnswer(atMs: number) {
	return { online: 0, fetched_at: new Date(atMs).toISOString(), relay_set: [], fresh: [] as { npub: string; seen_at: string }[] };
}

/** An answered payload that just saw OUR peer's beacon (⇒ pill reads Online). */
function onlineAnswer(atMs: number) {
	return { online: 1, fetched_at: new Date(atMs).toISOString(), relay_set: [], fresh: [{ npub: NPUB, seen_at: new Date(atMs).toISOString() }] };
}

// Fake timers drive the page's two clocks (the 20 s online poll and the 30 s presence clock);
// `advanceTimersByTimeAsync` runs due timers AND drains the microtask queues the async poll
// chain needs, so no `waitFor` polling is required (waitFor cannot advance fake timers here).
async function flush() {
	await vi.advanceTimersByTimeAsync(0);
	await vi.advanceTimersByTimeAsync(0);
	await vi.advanceTimersByTimeAsync(0);
}

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.clearAllMocks();
	contacts.set([]);
	contactsLoadError.set(false);
});

describe('QURATOR-338 — presence updates IN PLACE on a poll tick, without a remount', () => {
	it('a peer coming ONLINE while parked: mount answers Offline, the next tick answers Online — the pill flips', async () => {
		vi.useFakeTimers();
		vi.setSystemTime(BASE);
		contacts.set([peer({ last_presence: new Date(BASE - 3_600_000).toISOString() })]);

		// The seam the real backend sits behind: the poll's return value. Offline at mount…
		let answer = offlineAnswer(BASE);
		vi.mocked(onlineCount).mockImplementation(async () => answer);

		const { container } = render(ContactsPage);
		await flush();

		// …baseline: the pill reads Offline from the answered-but-empty fresh set.
		expect(container.querySelectorAll('.pill-online').length).toBe(0);
		expect(container.querySelectorAll('.pill-offline').length).toBe(1);

		// One 20 s tick later the backend's answer carries the peer's beacon. NO remount —
		// the same mounted page must re-derive and flip the pill.
		answer = onlineAnswer(BASE + 20_000);
		await vi.advanceTimersByTimeAsync(20_000);
		await flush();

		expect(container.querySelectorAll('.pill-online').length).toBe(1);
		expect(container.querySelectorAll('.pill-offline').length).toBe(0);
	});

	it('a peer going OFFLINE while parked: once the beacon ages past the window the pill flips in place (local clock)', async () => {
		vi.useFakeTimers();
		vi.setSystemTime(BASE);
		contacts.set([peer({ online: true, last_presence: new Date(BASE).toISOString() })]);

		let answer = onlineAnswer(BASE);
		vi.mocked(onlineCount).mockImplementation(async () => answer);

		const { container } = render(ContactsPage);
		await flush();

		// Baseline: the fresh set holds a live beacon ⇒ Online pill, row in the Online-now bucket.
		expect(container.querySelectorAll('.pill-online').length).toBe(1);

		// The peer stops publishing; the poll keeps answering "answered, no beacon". Inside the
		// 480 s window the persisted `last_presence` honestly still holds them online — the
		// next-tick flip here would be WRONG. The DESIGNED in-place prune is the local `nowMs`
		// clock aging the beacon past the window, so advance past it and demand the flip.
		answer = offlineAnswer(BASE + 20_000);
		await vi.advanceTimersByTimeAsync(600_000);
		await flush();

		expect(container.querySelectorAll('.pill-offline').length).toBe(1);
		expect(container.querySelectorAll('.pill-online').length).toBe(0);
	});
});
