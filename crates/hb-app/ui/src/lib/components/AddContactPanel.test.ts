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

/** Wrap hits in the M20 W3 result envelope `{ hits, capped }` (the shape `search_peers` returns). */
function resultEnvelope(hits: PeerSearchHit[], capped = false) {
	return { hits, capped };
}

/** Open the Discover section and run a search so the hit-cards render. Returns scoped query
 *  helpers bound to the rendered panel. */
async function discoverHits(hits: PeerSearchHit[], props: Record<string, unknown> = {}, capped = false) {
	(searchPeers as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(resultEnvelope(hits, capped));
	const { getByRole, getAllByRole, findByRole, findByText, queryByText, getByPlaceholderText } = render(AddContactPanel, {
		props: { open: true, ...props },
	});
	await fireEvent.click(getByRole('button', { name: /discover hoarders/i }));
	// QURATOR-44 broadened this placeholder from "tags (e.g. …)" to "name, bio, or tag (e.g. …)",
	// so the old /tags/i locator (plural) no longer matched and every test driving this flow failed
	// on the lookup rather than on its own assertion. Matched on the singular stem, which holds
	// across both wordings. The placeholder's exact copy is pinned in discover-view.test.ts — this
	// is only a locator, so it deliberately does NOT re-assert the copy here.
	await fireEvent.input(getByPlaceholderText(/tag/i), { target: { value: 'anime' } });
	await fireEvent.click(getByRole('button', { name: /^search$/i }));
	// Wait for the hit-card's Add-contact button (class hit-follow) to appear — that means searchPeers
	// resolved and the results rendered.
	const addBtn = await findByRole('button', { name: 'Add contact' });
	return { addBtn, getAllByRole, findByRole, findByText, queryByText };
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

describe('AddContactPanel — M20 W3 truncation affordance', () => {
	// QURATOR-44 replaced the "showing first N" wording with pagination plus a cap notice, because
	// "showing first N" was the never-ending-list symptom the owner asked to remove. The AFFORDANCE
	// must survive that copy change: a capped result still has to tell the user more matches exist
	// rather than presenting the slice as everyone. These two tests were re-pointed at the new
	// wording, NOT deleted — and note the negative case had silently become vacuous, since the old
	// /showing first/ string is absent whether or not the result is capped.
	it('tells the user more matches exist when the result is capped', async () => {
		// One hit returned, but the backend flagged more candidates existed (capped=true). Hit count
		// is independent of the cap flag — one hit + capped=true still means "there are more".
		const { findByText } = await discoverHits([makeHit()], {}, true);
		const affordance = await findByText(/more matches exist/i);
		expect(affordance).toBeTruthy();
		expect(affordance.getAttribute('role')).toBe('status');
	});

	it('shows no cap notice when the result is not capped', async () => {
		// capped=false → no notice. Mutation probe: rendering the notice unconditionally reds this.
		const { queryByText } = await discoverHits([makeHit()], {}, false);
		expect(queryByText(/more matches exist/i)).toBeNull();
	});
});

