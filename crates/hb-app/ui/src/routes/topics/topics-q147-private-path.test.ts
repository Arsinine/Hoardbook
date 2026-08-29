// @vitest-environment jsdom
// QURATOR-147 W5 — private Topics obey the public path convention (owner ruling 2026-08-27). Before
// W5 the create modal gated the root picker on `!newPrivate`, so a private Topic was a free-text
// name and obeying the convention meant knowing to type `video/foo` by hand — which is why legacy
// private Topics look like `back room`. W5: the SAME root + sub-path picker serves both, and a
// rootless legacy name lands under `other`, never under a singleton group of its own.
//
// Mount tests (render(Page) + fireEvent, mocking ONLY $lib/api.js — the established pattern).
// Each case names its mutation probe (P-10: break the production half on purpose, watch it red).
//
// jsdom computes no layout — nothing here proves a row RENDERS as one line; only that the picker,
// the composed name, the group placement, and the badge text are right.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({
		topic_id: 'p-new',
		name: 'other/back room',
		description: '',
		tags: [],
		private: true,
		joined_at: 0,
	}),
	topicUpdateMeta: vi.fn(),
	topicDiscoverPaint: vi.fn().mockResolvedValue([]),
	topicRank: vi.fn().mockResolvedValue([]),
	topicLookup: vi.fn().mockResolvedValue({ topic_id: '', name: '', exists: false, member_count_estimate: 0 }),
	topicJoinPublic: vi.fn(),
	topicRedeemInvite: vi.fn(),
	topicPreviewInvite: vi.fn(),
	topicLeave: vi.fn(),
	topicInvite: vi.fn(),
	topicRoster: vi.fn().mockResolvedValue([]),
	topicAnnounce: vi.fn(),
	topicAnnounceStatus: vi.fn().mockResolvedValue(0),
}));

import { topicList, topicCreate, topicDiscoverPaint } from '$lib/api.js';
const listMock = topicList as unknown as ReturnType<typeof vi.fn>;
const createMock = topicCreate as unknown as ReturnType<typeof vi.fn>;
const paintMock = topicDiscoverPaint as unknown as ReturnType<typeof vi.fn>;

/** A joined private Topic (local record). */
function privateMine(topic_id: string, name: string) {
	return { topic_id, name, description: '', tags: [], private: true, joined_at: 0 };
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-147 W5 — the create form offers the path picker for private too', () => {
	it('a private create composes root + sub-path and submits it with private: true', async () => {
		const { getByRole, getByPlaceholderText, container } = render(TopicsPage);

		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		// No freeform name field anymore, for private or public: the create form's only text inputs
		// are the sub-path and the description — the root picker + sub-path compose the name. (The
		// form's third input is the Private checkbox.)
		const formInputs = container.querySelectorAll('.create-fields input:not([type="checkbox"])');
		expect(formInputs.length).toBe(2);

		const check = container.querySelector<HTMLInputElement>('input[type="checkbox"]');
		expect(check).toBeTruthy();
		await fireEvent.click(check!);
		const select = container.querySelector<HTMLSelectElement>('.path-row select');
		expect(select).toBeTruthy();
		await fireEvent.change(select!, { target: { value: 'other' } });
		await fireEvent.input(getByPlaceholderText(/sub-path/i), { target: { value: 'back room' } });
		await tick();

		// The preview shows the composed path.
		expect(container.textContent).toContain('other/back room');

		const createBtn = [...container.querySelectorAll('button')].find(
			(b) => (b.textContent ?? '').trim().toLowerCase() === 'create',
		) as HTMLButtonElement;
		expect(createBtn).toBeTruthy();
		expect(createBtn.disabled).toBe(false);
		await fireEvent.click(createBtn);
		await tick();

		expect(createMock).toHaveBeenCalledTimes(1);
		expect(createMock).toHaveBeenCalledWith('other/back room', '', true);
		// A private create is unlisted: it never re-paints the directory.
		expect(paintMock).toHaveBeenCalledTimes(1);
	});
});

describe('QURATOR-147 W5 — a private Topic lands in its root group with the shared badge', () => {
	it('a picker-made private Topic renders under its root group with the .hb-tag private badge', async () => {
		// `other/back room` — what the picker composes. The group is `other` (open by the join rule).
		paintMock.mockResolvedValue([]);
		listMock.mockResolvedValue([privateMine('p1', 'other/back room')]);
		const { container } = render(TopicsPage);

		const row = await waitFor(() => {
			const el = [...container.querySelectorAll('.topic-row .name')].find((n) =>
				(n.textContent ?? '').includes('other/back room'),
			);
			expect(el).toBeTruthy();
			return el!;
		});
		// The shared global badge (never a page-local copy) rides the row.
		const badge = row.querySelector('.hb-tag');
		expect(badge?.textContent?.trim()).toBe('private');
		// It sits under the `other` root group — not a singleton group of its own.
		const header = [...container.querySelectorAll('.root-header .root-name')].map((n) => n.textContent);
		expect(header).toEqual(['other']);
	});

	it('a rootless LEGACY private Topic (pre-W5 name, no slash) lands under other — not its own group', async () => {
		// Pre-W5 reality: a private Topic created with the old freeform field, name `back room`.
		// Mutate `topicRootOf` back to `splitTopicPath(name)[0] ?? 'other'` (or `name.split('/')[0]`)
		// and this reds on the `header` assertion: 'back room' becomes its own singleton group.
		paintMock.mockResolvedValue([]);
		listMock.mockResolvedValue([privateMine('p2', 'back room')]);
		const { container } = render(TopicsPage);

		await waitFor(() => expect(container.querySelectorAll('.topic-row').length).toBe(1));
		const header = [...container.querySelectorAll('.root-header .root-name')].map((n) => n.textContent);
		expect(header).toEqual(['other']);
		// The row is drawn under `other` (open: something is joined under it).
		expect(container.querySelector('.topic-row .name')?.textContent).toContain('back room');
	});
});
