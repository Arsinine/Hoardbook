// @vitest-environment jsdom
// QURATOR-342 Lane C — the People-like-you panel. Behavioural tests: the real component mounts,
// only `$lib/api.js` is mocked (topics-q83-empty-refetch.test.ts pattern); the `contacts` store is
// the REAL writable, seeded per test. jsdom computes no layout, so the "3 across" ruling is pinned
// on the grid's inline style attribute (the one DOM-visible carrier of the rule).
//
// Every test below was SEEN RED against the exact production mutation named in its comment, then
// restored (cp backup + touch) and re-run green (see PeopleLikeYou.test mutations log in the lane
// report). Restore key: the full path — never basename (P-27).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, fireEvent, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';

vi.mock('$lib/api.js', () => ({
	similarPeople: vi.fn(),
	topicDiscoverPaint: vi.fn(),
	getProfile: vi.fn(),
	saveProfile: vi.fn(),
	hasPublishedProfile: vi.fn(),
	publishProfile: vi.fn(),
	follow: vi.fn(),
}));

import PeopleLikeYou from './PeopleLikeYou.svelte';
import {
	similarPeople,
	topicDiscoverPaint,
	getProfile,
	saveProfile,
	hasPublishedProfile,
	publishProfile,
} from '$lib/api.js';
import { contacts, profile as profileStore } from '$lib/stores.js';
import { fmtLargestUnit } from '$lib/browse-view.js';
import type { ContactSummary, PeopleResult, Profile, SimilarPerson } from '$lib/types.js';

const similarPeopleMock = similarPeople as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;
const getProfileMock = getProfile as unknown as ReturnType<typeof vi.fn>;
const saveProfileMock = saveProfile as unknown as ReturnType<typeof vi.fn>;
const hasPubMock = hasPublishedProfile as unknown as ReturnType<typeof vi.fn>;
const publishMock = publishProfile as unknown as ReturnType<typeof vi.fn>;

beforeEach(() => {
	contacts.set([]);
	profileStore.set(null);
	similarPeopleMock.mockReset();
	paintMock.mockReset();
	getProfileMock.mockReset();
	saveProfileMock.mockReset();
	hasPubMock.mockReset();
	publishMock.mockReset();
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
	profileStore.set(null);
});

const NPUB_T = 'npub1titlesqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq';
const NPUB_I = 'npub1interestssssssssssssssssssssssssssssss';
const NPUB_A = 'npub1aliceeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';

const person = (over: Partial<SimilarPerson> & { npub: string }): SimilarPerson => ({
	display_name: 'Someone',
	fingerprint: { words: ['amber', 'cedar', 'jade', 'quartz', 'tarn'], colorHex: '#f00' },
	score: 0,
	shared_titles: 0,
	shared_interests: [],
	reason: 'titles_in_common',
	...over,
});

const contact = (npub: string, over: Partial<ContactSummary> = {}): ContactSummary =>
	({
		npub,
		has_browse_key: true,
		collections: [],
		online: true,
		last_fetched: '2026-09-28T00:00:00Z',
		local_tags: [],
		score: 0,
		shared_titles: 0,
		shared_interests: [],
		reason: 'titles_in_common',
		...over,
	}) as ContactSummary;

const profileOf = (over: Partial<Profile> = {}): Profile => ({
	display_name: 'Alice Park',
	tags: ['films'],
	languages: [],
	social_links: [],
	willing_to: [],
	content_types: ['video'],
	updated: '2026-09-28T00:00:00Z',
	...over,
});

const ok = (people: SimilarPerson[]): PeopleResult => ({ people, cold_start: false });

describe('PeopleLikeYou — populated', () => {
	it('renders both groups and pins the exactly-3-columns grid style on every grid', async () => {
		// SEEN RED: deleted `style="grid-template-columns: repeat(3, 1fr);"` from both .ply-grid divs
		// in PeopleLikeYou.svelte — both style assertions failed. Restored (cp + touch), green again.
		similarPeopleMock.mockResolvedValue(
			ok([
				person({ npub: NPUB_T, display_name: 'Alice Park', shared_titles: 12 }),
				person({ npub: NPUB_I, display_name: 'Bob Ion', reason: 'interests_only', shared_interests: ['films', 'synth'] }),
			]),
		);
		const { container } = render(PeopleLikeYou);

		await waitFor(() => expect(container.querySelector('.ply-grid')).toBeTruthy());
		expect(container.textContent).toContain('Most titles in common');
		expect(container.textContent).toContain('Close on Interests');
		// Rank in words.
		expect(container.textContent).toContain('12 titles in common');
		expect(container.textContent).toContain('In common: films, synth');
		const grids = container.querySelectorAll('.ply-grid');
		expect(grids.length).toBe(2);
		for (const g of grids) {
			expect(g.getAttribute('style')).toContain('repeat(3, 1fr)');
		}
	});

	it('locked person shows the size-rule line and no Browse affordance', async () => {
		// SEEN RED: changed the locked branch to render the Browse link too — "Browse" appeared and
		// the test failed. Restored (cp + touch), green again.
		const need = Math.round(44.5 * 1024 ** 4);
		similarPeopleMock.mockResolvedValue(
			ok([person({ npub: NPUB_T, display_name: 'Tomás Ruiz', read_state: { kind: 'locked', need_bytes: need } })]),
		);
		const { container } = render(PeopleLikeYou);

		const line = await waitFor(() => {
			const el = Array.from(container.querySelectorAll('.ply-locked')).find((e) =>
				e.textContent?.startsWith('Readable once your hoard reaches'),
			);
			expect(el).toBeTruthy();
			return el!;
		});
		expect(line.textContent).toBe(`Readable once your hoard reaches ${fmtLargestUnit(need)}`);
		expect(container.textContent).not.toContain('Browse');
	});

	it('readable non-contact shows Browse and the + Add affordance', async () => {
		// SEEN RED: deleted the `{#if !$contacts.some(…)} … + Add … {/if}` block from the card —
		// the Add button vanished and the test failed. Restored (cp + touch), green again.
		similarPeopleMock.mockResolvedValue(ok([person({ npub: NPUB_T, display_name: 'Alice Park', read_state: { kind: 'readable' } })]));
		const { container, getByText } = render(PeopleLikeYou);

		const add = await waitFor(() => {
			const b = Array.from(container.querySelectorAll('button')).find((x) => x.textContent?.trim() === '+ Add');
			expect(b).toBeTruthy();
			return b!;
		});
		expect(getByText('Browse')).toBeTruthy();
		expect((add as HTMLButtonElement).disabled).toBe(false);
	});

	it('+ Add disappears when the contacts store later gains that person', async () => {
		// Review finding (orchestrator, 2026-09-28): the contact check read a one-shot get(contacts)
		// snapshot, so a roster refresh never hid "+ Add". P-10 MUTATION: in PeopleLikeYou.svelte
		// replace `$contacts.some((x) => x.npub === p.npub)` in the Add guard with `false` — the
		// button survives the store update and this test REDS.
		contacts.set([]);
		similarPeopleMock.mockResolvedValue(ok([person({ npub: NPUB_T, display_name: 'Alice Park', read_state: { kind: 'readable' } })]));
		const { container } = render(PeopleLikeYou);
		const addBtn = () => Array.from(container.querySelectorAll('button')).find((x) => x.textContent?.trim() === '+ Add');
		await waitFor(() => expect(addBtn()).toBeTruthy());
		contacts.set([contact(NPUB_T)]);
		await waitFor(() => expect(addBtn()).toBeUndefined());
	});

	it('hovering a card opens the profile popover carrying a teaser collection name', async () => {
		// SEEN RED: deleted the teaser_collections section from the popover — "hand-press" never
		// appeared and the test failed. Restored (cp + touch), green again.
		contacts.set([
			contact(NPUB_A, {
				profile: profileOf({
					bio: 'Archives of hand-printed maps.',
					teaser_collections: [
						{ name: 'hand-press', bytes: 2.5 * 1024 ** 3 },
						{ name: 'darkroom', bytes: 6 * 1024 ** 3 },
					],
				}),
			}),
		]);
		similarPeopleMock.mockResolvedValue(ok([person({ npub: NPUB_A, display_name: 'Alice Park', shared_titles: 3 })]));
		const { container } = render(PeopleLikeYou);

		await waitFor(() => expect(container.querySelector('.ply-card')).toBeTruthy());
		expect(container.querySelector('[role="tooltip"]')).toBeNull();
		fireEvent.mouseEnter(container.querySelector('.ply-card')!);

		const pop = await waitFor(() => {
			const p = container.querySelector('[role="tooltip"]');
			expect(p).toBeTruthy();
			return p!;
		});
		expect(pop.textContent).toContain('hand-press');
		expect(pop.textContent).toContain(fmtLargestUnit(2.5 * 1024 ** 3));
		expect(pop.textContent).toContain('Archives of hand-printed maps.');
	});
});

describe('PeopleLikeYou — cold start', () => {
	const topics = [
		{ topic_id: 't1', name: 'video/analog/synth', description: '', tags: ['films', 'synth'], member_count_estimate: null },
		{ topic_id: 't2', name: 'radio/quartz-radio', description: '', tags: ['maps'], member_count_estimate: null },
	];

	it('renders the chip wall retrieved from topics, including a chip that exists only in the mock', async () => {
		// SEEN RED: removed `bump(t.name)` from chipWall (tags only) — the mock-only NAME chip
		// "radio/quartz-radio" vanished and the test failed. Restored (cp + touch), green again.
		similarPeopleMock.mockResolvedValue({ people: [], cold_start: true });
		paintMock.mockResolvedValue(topics);
		const { container } = render(PeopleLikeYou);

		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		expect(paintMock.mock.calls[0][0]).toEqual(['video', 'audio', 'image', 'text', 'software', 'other']);
		await waitFor(() => {
			const chips = Array.from(container.querySelectorAll('.ply-chip')).map((c) => c.textContent?.trim());
			expect(chips).toContain('radio/quartz-radio'); // exists ONLY in the mock — a NAME, not a tag
			expect(chips).toContain('films');
			expect(chips).toContain('synth');
			expect(chips).toContain('maps');
		});
		expect(container.textContent).toContain('Add a collection');
	});

	it('an empty discovery renders the actions with NO chips and never a hardcoded fallback list', async () => {
		// SEEN RED: added a hardcoded fallback in loadChips (`chips.length ? chips : ['films','music','games']`)
		// — the fallback chips rendered and the zero-chip assertion failed. Restored (cp + touch), green again.
		similarPeopleMock.mockResolvedValue({ people: [], cold_start: true });
		paintMock.mockResolvedValue([]);
		const { container } = render(PeopleLikeYou);

		await waitFor(() => expect(container.textContent).toContain('Add a collection'));
		await waitFor(() => expect(paintMock).toHaveBeenCalledTimes(1));
		// Flush the resolved paint promise into the DOM before asserting absence — the chips
		// assignment lands in a promise continuation, and an unflushed assert passes vacuously.
		await tick();
		await tick();
		expect(container.querySelectorAll('.ply-chip').length).toBe(0);
	});

	it('clicking a chip appends the tag to my profile, republishes, and re-runs the ranking', async () => {
		// SEEN RED: mutated addChip to `await saveProfile(base)` (the UNCHANGED profile, tag never
		// appended) — the appended-tags assertion failed. Restored (cp + touch), green again.
		similarPeopleMock.mockResolvedValue({ people: [], cold_start: true });
		paintMock.mockResolvedValue(topics);
		getProfileMock.mockResolvedValue(profileOf({ tags: ['films'] }));
		hasPubMock.mockResolvedValue(true);
		const { container, getByText } = render(PeopleLikeYou);

		const chip = await waitFor(() => {
			const c = Array.from(container.querySelectorAll('.ply-chip')).find((x) => x.textContent?.trim() === 'synth');
			expect(c).toBeTruthy();
			return c!;
		});
		fireEvent.click(chip);

		await waitFor(() => expect(saveProfileMock).toHaveBeenCalledTimes(1));
		const saved = saveProfileMock.mock.calls[0][0] as Profile;
		expect(saved.tags).toEqual(['films', 'synth']);
		await waitFor(() => expect(publishMock).toHaveBeenCalledTimes(1));
		await waitFor(() => expect(similarPeopleMock).toHaveBeenCalledTimes(2));
		expect(getByText('Add a collection')).toBeTruthy();
	});
});

describe('PeopleLikeYou — failure', () => {
	it('a failed similarPeople shows the error state with Retry and never the empty copy', async () => {
		// SEEN RED: mutated load()'s catch to `result = { people: [], cold_start: false }` (swallow)
		// — Retry never rendered and the empty copy appeared; the test failed. Restored (cp + touch), green again.
		similarPeopleMock.mockRejectedValue(new Error('relay down'));
		const { container, getByText, queryByText } = render(PeopleLikeYou);

		await waitFor(() => expect(getByText('Retry')).toBeTruthy());
		expect(container.textContent).not.toContain('Nothing to compare yet');
		expect(queryByText('People like you could not be loaded.')).toBeTruthy();
	});
});
