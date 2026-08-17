// @vitest-environment jsdom
// QURATOR-97 — the create-Topic modal holds typed state (name / sub-path / description); a stray
// backdrop click silently discards it. The modal must not close on backdrop (closeOnBackdrop=false);
// Cancel and Escape stay deliberate actions.
//
// This is a BEHAVIOURAL mount (open the real modal, click the real backdrop), not a source-scan:
// the question is what a click does, and only running the component answers it. The typed-state
// half is pinned directly — the input's value must SURVIVE the backdrop click.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn(),
	topicUpdateMeta: vi.fn(),
	topicDiscover: vi.fn(),
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

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-97 — create-Topic modal ignores backdrop clicks', () => {
	it('a backdrop click neither closes the modal nor wipes the typed sub-path', async () => {
		const { getByRole, getByPlaceholderText } = render(TopicsPage);

		// Open the create modal.
		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		const dialog = getByRole('dialog');
		expect(dialog).toBeTruthy();

		// Type something worth protecting.
		const subPath = getByPlaceholderText(/sub-path/i);
		await fireEvent.input(subPath, { target: { value: 'animation/anime' } });

		// Click the backdrop itself (target === currentTarget), not the card.
		const backdrop = document.querySelector('.modal-backdrop') as HTMLElement;
		expect(backdrop).toBeTruthy();
		await fireEvent.click(backdrop);
		await tick();

		// The modal is still open and the typed value survived.
		expect(getByRole('dialog')).toBeTruthy();
		expect((getByPlaceholderText(/sub-path/i) as HTMLInputElement).value).toBe('animation/anime');
	});

	it('Cancel stays a deliberate close (the affordance the ticket keeps)', async () => {
		const { getByRole } = render(TopicsPage);
		await fireEvent.click(getByRole('button', { name: /\+ new topic/i }));
		await tick();
		await fireEvent.click(getByRole('button', { name: /cancel/i }));
		await waitFor(() => expect(() => getByRole('dialog')).toThrow());
	});
});
