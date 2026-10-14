// @vitest-environment jsdom
// QURATOR-378 rev b — the contact pane redesign the owner signed off (draft 2 rev b, 2026-10-14):
// no cards; four tiers read top-down (who they are, what they share, details, the Private box);
// groups leave the pane; Collects becomes Tags, where YOUR tags glow beside their grey ones; past 5
// public collections the list stops behind a "Show N more" text link that expands IN PLACE.
//
// Mounted page (P-4), only `$lib/api.js` mocked. ⚠ P-13: jsdom computes no layout and does not
// inject this component's <style>, so the glow, the type sizes and the tinted box are CSS claims
// proven in vite + Playwright, not here. This file pins ORDER, MEMBERSHIP and the cap's behaviour.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary, Collection, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	refreshContact: vi.fn().mockResolvedValue(undefined),
	setContactTags: vi.fn().mockResolvedValue(undefined),
	groupsGet: vi.fn().mockResolvedValue([]),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: null, relay_set: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	privateAudienceSet: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const NPUB = 'npub1q378tiers' + 't'.repeat(46);

function cols(n: number): Collection[] {
	return Array.from({ length: n }, (_, i) => ({
		slug: `c${i}`, path_alias: `Collection ${i + 1}`, visibility: 'Public', est_size: '1 GB', description: null,
	})) as unknown as Collection[];
}

function peer(nCols: number, extra: Partial<ContactSummary> = {}): ContactSummary {
	return {
		npub: NPUB,
		has_browse_key: true,
		collections: cols(nCols),
		online: false,
		last_fetched: new Date().toISOString(),
		local_tags: ['met at con'],
		profile: {
			display_name: 'Tiered Tess', bio: 'Laserdisc restorations.', looking_for: 'Madhouse OVAs',
			tags: ['anime'], content_types: ['video'], languages: ['en'], social_links: [], willing_to: [],
			updated: '2026-08-01T00:00:00Z',
		} as unknown as Profile,
		...extra,
	} as ContactSummary;
}

async function open(p: ContactSummary): Promise<HTMLElement> {
	contacts.set([p]);
	const { container } = render(ContactsPage);
	await waitFor(() => expect(container.querySelector('.contact-card')).not.toBeNull());
	fireEvent.click(container.querySelector('.contact-card')!);
	await waitFor(() => expect(container.querySelector('.detail-pane .about')).not.toBeNull());
	return container.querySelector('.detail-pane') as HTMLElement;
}

afterEach(() => {
	cleanup();
	contacts.set([]);
});

describe('QURATOR-378 rev b — the contact pane in four tiers', () => {
	it('tiers in order, no wrapper card: about → shares → details → the Private box', async () => {
		// P-10 MUTATION (orchestrator): move the `<section class="facts-sec">` block above the
		// `{#if publicCollections.length > 0}` shares block → REDS (order).
		const pane = await open(peer(2));
		const scroll = pane.querySelector('.detail-scroll')!;
		expect(pane.querySelector('.contact-detail'), 'the middle container came back').toBeNull();
		expect(pane.querySelector('.detail-block'), 'a card block came back').toBeNull();
		const order = [...scroll.children].map((el) => el.className.split(' ').find((c) => ['about', 'shares', 'facts-sec', 'mine'].includes(c))).filter(Boolean);
		expect(order).toEqual(['about', 'shares', 'facts-sec', 'mine']);
		// The Private toggle is the box's content (owner: "Keep private toggle in the box").
		expect(scroll.querySelector('.mine')!.textContent).toContain('Receives my Private collections');
	});

	it('Tags: their types and tags are grey, your tag glows, in ONE row; Identity is the last fact', async () => {
		const pane = await open(peer(1));
		const tags = pane.querySelector('.about .tags')!;
		expect([...tags.querySelectorAll('.peer-tag')].map((t) => t.textContent)).toEqual(['video', 'anime']);
		const mine = tags.querySelector('.local-tag')!;
		expect(mine.textContent).toContain('met at con');
		expect(mine.classList.contains('peer-tag')).toBe(false);
		expect(tags.querySelector('.tag-add-btn')).not.toBeNull();
		const dts = [...pane.querySelectorAll('.facts-sec dt')].map((d) => d.textContent);
		expect(dts[dts.length - 1]).toBe('Identity');
	});

	it('5 or fewer collections show all of them and no Show-more link', async () => {
		const pane = await open(peer(5));
		expect(pane.querySelectorAll('.cpop-row').length).toBe(5);
		expect(pane.querySelector('.show-more')).toBeNull();
	});

	it('past 5 collections the list stops at 5; "Show N more" expands in place and back', async () => {
		// P-10 MUTATION (orchestrator): set `const PANE_COLS_SHOWN = 5;` to `= 500;` → REDS (12 rows
		// render at once and there is no link).
		const pane = await open(peer(12));
		expect(pane.querySelectorAll('.cpop-row').length).toBe(5);
		// The heading's count stays the honest total while rows are capped.
		expect(pane.querySelector('.shares .sec-count')!.textContent).toBe('12');
		const more = pane.querySelector('.show-more') as HTMLButtonElement;
		expect(more.textContent!.trim()).toBe('Show 7 more');
		await fireEvent.click(more);
		expect(pane.querySelectorAll('.cpop-row').length).toBe(12);
		expect(more.textContent!.trim()).toBe('Show fewer');
		await fireEvent.click(more);
		expect(pane.querySelectorAll('.cpop-row').length).toBe(5);
		// The disclosure footer stays under the list.
		expect(pane.querySelector('.shares .cpop-footer')).not.toBeNull();
	});
});
