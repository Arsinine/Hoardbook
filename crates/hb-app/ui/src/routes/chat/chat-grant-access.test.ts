// @vitest-environment jsdom
// QURATOR-160 send side — the one-click grant affordance: a RECOGNISED access request (the
// `{"hb":"access_request",…}` body QURATOR-137 slice 2 defined) renders a "Grant access" button
// in the chat thread, and one press fires the `grant_browse_access` Tauri command with the
// MESSAGE SENDER's npub — never whoever happens to be selected when the click lands. A plain
// prose message from the same peer renders NO button: recognition gates the grant, not presence.
//
// The page is mounted for real (only `$lib/api.js` + `$app/*` are mocked — the CLAUDE.md recipe;
// `importOriginal` spreads the real module so every transitive named import still resolves, then
// overrides only what this page drives). jsdom computes no layout — this proves the affordance
// and the wired call, not pixel placement.
//
// MUTATION (P-10, cheap — APPLIED AND REVERTED by this lane, observed red): in `+page.svelte`,
// change the grant button's guard from `{#if !isMe && parseAccessRequest(msg.content)}` to
// `{#if !isMe}` — the second test reds ("expected <button> to be undefined"). A second red:
// pass `myId` instead of `msg.from` to `handleGrantAccess` — the first test reds on
// `toHaveBeenCalledWith(ASKER)`.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import ChatPage from './+page.svelte';

// vi.mock factories are hoisted above every const — the values they reference must come from
// vi.hoisted, not plain top-level consts (ReferenceError otherwise).
const { ME, ASKER, ACCESS_BODY } = vi.hoisted(() => {
	const asker = 'npub1asker'.padEnd(63, 'a');
	return {
		ME: 'npub1me'.padEnd(63, 'm'),
		ASKER: asker,
		ACCESS_BODY: JSON.stringify({
			hb: 'access_request',
			v: 1,
			asker_npub: asker,
			nonce: 'n-1',
			requested_at: 1,
		}),
	};
});

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$app/stores', () => ({
	page: {
		subscribe: (run: (v: unknown) => void) => {
			// ?peer= opens the conversation via the deep-link effect (contact branch — no pasteKey).
			run({ url: new URL(`http://local/chat?peer=${ASKER}`), params: {}, route: { id: '/chat' } });
			return () => {};
		},
	},
}));

vi.mock('$lib/api.js', async (importOriginal) => ({
	...(await importOriginal<Record<string, unknown>>()),
	getMessages: vi.fn().mockResolvedValue([
		{ from: ASKER, to: ME, content: ACCESS_BODY, sent_at: '2026-09-10T10:00:00Z' },
	]),
	grantBrowseAccess: vi.fn().mockResolvedValue(undefined),
	getContacts: vi.fn().mockResolvedValue([{ npub: ASKER, petname: 'Asker', groups: [] }]),
	groupsGet: vi.fn().mockResolvedValue([]),
	getCollections: vi.fn().mockResolvedValue([]),
	getSettings: vi.fn().mockResolvedValue({}),
	dmRequests: vi.fn().mockResolvedValue([]),
	relayStatus: vi.fn().mockResolvedValue([]),
	topicList: vi.fn().mockResolvedValue([]),
	advanceReadWatermark: vi.fn().mockResolvedValue(undefined),
}));

import { grantBrowseAccess, getMessages } from '$lib/api.js';
const grantMock = grantBrowseAccess as unknown as ReturnType<typeof vi.fn>;
const msgsMock = getMessages as unknown as ReturnType<typeof vi.fn>;
import { identity, contacts } from '$lib/stores.js';

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

function mount() {
	identity.set({ npub: ME });
	contacts.set([{ npub: ASKER, petname: 'Asker', groups: [] }]);
	return render(ChatPage);
}

const grantButton = () =>
	[...document.querySelectorAll('button')].find(
		(b) => (b.textContent ?? '').trim() === 'Grant access'
	);

describe('QURATOR-160 send side — the Grant access affordance', () => {
	it('renders on a recognised request; one click grants to the SENDER npub', async () => {
		mount();
		const btn = await waitFor(() => {
			const b = grantButton();
			expect(b).toBeTruthy();
			return b as HTMLButtonElement;
		}, { timeout: 3000 });
		await fireEvent.click(btn);
		await waitFor(() => expect(grantMock).toHaveBeenCalledTimes(1));
		expect(grantMock).toHaveBeenCalledWith(ASKER);
	});

	it('a plain prose message from the same peer renders NO Grant access button', async () => {
		msgsMock.mockResolvedValue([
			{ from: ASKER, to: ME, content: 'hey, can I browse your shelf?', sent_at: '2026-09-10T10:00:00Z' },
		]);
		mount();
		// Give the inbox poll a moment to land the prose message, then assert the affordance
		// never appears.
		await waitFor(() => expect(msgsMock).toHaveBeenCalled());
		await new Promise((r) => setTimeout(r, 100));
		expect(grantButton()).toBeUndefined();
	});
});
