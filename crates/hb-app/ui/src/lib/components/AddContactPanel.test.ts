// @vitest-environment jsdom
// M17 W1 — "Message" everywhere. The discovery hit-card (AddContactPanel's "Discover hoarders"
// section) gains a "Message" action alongside "Add contact". Add contact stays primary (first);
// Message routes to the `/chat?compose=<npub>` deep-link (works for non-contacts) and fires the
// `onmessage` callback rather than the `onadd` funnel.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import AddContactPanel from './AddContactPanel.svelte';
import { contacts } from '../stores.js';
import type { PeerSearchHit } from '../api.js';

vi.mock('../api.js', () => ({
	pasteKey: vi.fn(),
	searchPeers: vi.fn(),
}));

import { searchPeers } from '../api.js';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

function makeHit(overrides: Partial<PeerSearchHit> = {}): PeerSearchHit {
	return {
		npub: 'npub1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
		display_name: 'Stranger',
		bio: null,
		tags: ['anime'],
		content_types: ['video'],
		picture: null,
		fingerprint: { words: ['alpha', 'beta'], colorHex: '#f00' },
		...overrides,
	};
}

/** Open the Discover section and run a search so the hit-cards render. Returns scoped query
 *  helpers bound to the rendered panel. */
async function discoverHits(hits: PeerSearchHit[], props: Record<string, unknown> = {}) {
	(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(hits);
	const { getByRole, getAllByRole, findByRole, getByPlaceholderText } = render(AddContactPanel, {
		props: { open: true, ...props },
	});
	await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
	await fireEvent.input(getByPlaceholderText(/tags/i), { target: { value: 'anime' } });
	await fireEvent.click(getByRole('button', { name: /^search$/i }));
	// Wait for the hit-card's Add-contact button (class hit-follow) to appear — that means searchPeers
	// resolved and the results rendered.
	const addBtn = await findByRole('button', { name: 'Add contact' });
	return { addBtn, getAllByRole, findByRole };
}

describe('AddContactPanel — M17 W1 discovery Message action', () => {
	it('hit_card_renders_Add_contact_first_and_Message_second', async () => {
		const { addBtn, getAllByRole } = await discoverHits([makeHit()]);
		// The hit-card has both buttons (Add contact + Message); Add contact is primary/first.
		const msgBtns = getAllByRole('button', { name: 'Message' });
		expect(msgBtns.length).toBe(1);
		expect(addBtn).toBeTruthy();
		// Add contact precedes Message in document order.
		expect(addBtn.compareDocumentPosition(msgBtns[0]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
	});

	it('Message_fires_onmessage_with_npub_not_onadd', async () => {
		const onadd = vi.fn();
		const onmessage = vi.fn();
		const { getAllByRole } = await discoverHits([makeHit({ npub: 'npub1msgtarget' })], { onadd, onmessage });
		await fireEvent.click(getAllByRole('button', { name: 'Message' })[0]);
		expect(onmessage).toHaveBeenCalledTimes(1);
		expect(onmessage.mock.calls[0][0]).toBe('npub1msgtarget');
		// Add funnel is NOT triggered by Message.
		expect(onadd).not.toHaveBeenCalled();
	});
});
