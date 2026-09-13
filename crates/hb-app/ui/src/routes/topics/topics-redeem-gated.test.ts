// @vitest-environment jsdom
// Owner feedback #7 — "In Topics, Redeem a private topic invite should be hidden unless the user
// actually has an invite." The detection already existed (redeemInvite's preview), but it ran AFTER
// the click: the user was offered an action, clicked it, and was told it was never available. The
// page now probes topicPreviewInvite() once on mount and gates the button on the result, so with no
// invite the affordance is ABSENT FROM THE DOM (hidden, not disabled — the owner's word).
//
// Both arms are pinned because the whole point is the DIFFERENCE between the two states; a
// one-arm test would pass on an always-hidden button. A THROWING probe (offline, relay down) is
// pinned to the hidden arm too — an error path must not "helpfully" reveal the affordance.
//
// Per CLAUDE.md §9, a green test proves nothing until seen red on the broken code. The mutation
// probe (delete the `{#if hasPendingInvite}` wrapper around the Redeem button in +page.svelte,
// re-run this file) MUST fail 'no pending invite' and 'a throwing probe' here.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor, screen } from '@testing-library/svelte';
import TopicsPage from './+page.svelte';

vi.mock('$lib/api.js', () => ({
	topicList: vi.fn().mockResolvedValue([]),
	topicCreate: vi.fn().mockResolvedValue({
		topic_id: 't-new',
		name: 'video/animation/anime',
		description: '',
		tags: [],
		private: false,
		joined_at: 0,
	}),
	topicUpdateMeta: vi.fn(),
	topicDiscoverPaint: vi.fn(),
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

import { topicPreviewInvite } from '$lib/api.js';
const previewMock = topicPreviewInvite as unknown as ReturnType<typeof vi.fn>;

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

// Shape comes from redeemInvite's use of the preview: preview.name / preview.issuer_npub /
// preview.topic_id (the W8 consent modal + substitution-guard binding).
const preview = { name: 'The Back Room', issuer_npub: 'npub1issuer', topic_id: 't-priv-1' };

describe('owner feedback #7 — Redeem is hidden unless an invite is actually pending', () => {
	it('no pending invite: the Redeem affordance is absent from the DOM (hidden, not disabled)', async () => {
		previewMock.mockResolvedValue(null);
		const { queryByRole } = render(TopicsPage);
		// Wait for the mount probe to have RUN before asserting absence — an early query could
		// pass vacuously before the async probe even started.
		await waitFor(() => expect(previewMock).toHaveBeenCalled());
		await new Promise((r) => setTimeout(r, 50));
		expect(queryByRole('button', { name: /redeem/i })).toBeNull();
	});

	it('pending invite: the button renders and still opens the W8 consent modal', async () => {
		previewMock.mockResolvedValue(preview);
		const { getByRole } = render(TopicsPage);
		const btn = await waitFor(() => getByRole('button', { name: /redeem/i }));
		await fireEvent.click(btn);
		// The consent modal opens carrying the PREVIEWED topic name — the W8 flow (preview ->
		// consent -> confirmRedeem) is unchanged by the gating. screen.* searches document.body,
		// so a portal-rendered Modal is still found.
		expect(await screen.findByText(/The Back Room/)).toBeTruthy();
	});

	it('a throwing probe (offline, relay down) hides the button — an error is not an invite', async () => {
		previewMock.mockRejectedValue(new Error('relay down'));
		const { queryByRole } = render(TopicsPage);
		await waitFor(() => expect(previewMock).toHaveBeenCalled());
		await new Promise((r) => setTimeout(r, 50));
		expect(queryByRole('button', { name: /redeem/i })).toBeNull();
	});
});
