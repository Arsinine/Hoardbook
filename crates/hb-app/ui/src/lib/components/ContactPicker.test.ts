// @vitest-environment jsdom
// M21 W3 — the one contact-picker used by Topics → Invite and Chat → compose "+". Presentational:
// takes a contacts array + callbacks, owns the list + free-npub field, emits the chosen npub string.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import ContactPicker from './ContactPicker.svelte';
import type { CachedPeer } from '../types.js';

const here = dirname(fileURLToPath(import.meta.url));
const topicsSrc = () => readFileSync(resolve(here, '..', '..', 'routes', 'topics', '+page.svelte'), 'utf8');
const chatSrc = () => readFileSync(resolve(here, '..', '..', 'routes', 'chat', '+page.svelte'), 'utf8');

afterEach(cleanup);

function makePeer(overrides: Partial<CachedPeer> = {}): CachedPeer {
	return {
		npub: 'npub1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
		browse_key_hex: undefined,
		petname: undefined,
		profile: undefined,
		collections: [],
		online: false,
		last_fetched: '',
		local_tags: [],
		...overrides,
	};
}

const PEERS: CachedPeer[] = [
	makePeer({ npub: 'npub1alice', petname: 'Alice' }),
	makePeer({ npub: 'npub1bob', profile: { display_name: 'Bob', tags: [], languages: [], social_links: [], willing_to: [], content_types: [], updated: '' } }),
];

describe('ContactPicker (M21 W3) — contact list + selection', () => {
	it('renders the contact list with resolved display names', () => {
		const { getByText, getAllByRole } = render(ContactPicker, {
			props: { open: true, contacts: PEERS },
		});
		// Petname-first resolution (contactDisplayName): Alice via petname, Bob via display_name.
		expect(getByText('Alice')).toBeTruthy();
		expect(getByText('Bob')).toBeTruthy();
		// Both rows are options in the listbox.
		expect(getAllByRole('option').length).toBe(2);
	});

	it('selecting a contact row then confirm emits that contact npub', async () => {
		const onselect = vi.fn();
		const { getAllByRole, getByRole } = render(ContactPicker, {
			props: { open: true, contacts: PEERS, onselect },
		});
		await fireEvent.click(getAllByRole('option')[1]); // Bob
		await fireEvent.click(getByRole('button', { name: /select/i }));
		expect(onselect).toHaveBeenCalledTimes(1);
		expect(onselect.mock.calls[0][0]).toBe('npub1bob');
	});

	it('excludes the local user from the list', () => {
		const me = makePeer({ npub: 'npub1me', petname: 'Me' });
		const { queryByText, getAllByRole } = render(ContactPicker, {
			props: { open: true, contacts: [...PEERS, me], myNpub: 'npub1me' },
		});
		expect(queryByText('Me')).toBeNull();
		expect(getAllByRole('option').length).toBe(2); // Alice + Bob only
	});
});

describe('ContactPicker (M21 W3) — free-npub path', () => {
	it('typing a new npub and confirming emits the trimmed value', async () => {
		const onselect = vi.fn();
		const { getByPlaceholderText, getByRole } = render(ContactPicker, {
			props: { open: true, contacts: PEERS, onselect },
		});
		await fireEvent.input(getByPlaceholderText(/npub1/i), { target: { value: '  npub1newperson  ' } });
		await fireEvent.click(getByRole('button', { name: /select/i }));
		expect(onselect).toHaveBeenCalledTimes(1);
		expect(onselect.mock.calls[0][0]).toBe('npub1newperson');
	});

	it('confirm is disabled when nothing is chosen', () => {
		const { getByRole } = render(ContactPicker, {
			props: { open: true, contacts: PEERS },
		});
		expect((getByRole('button', { name: /select/i }) as HTMLButtonElement).disabled).toBe(true);
	});

	it('picking a contact then typing clears the selection (mutually exclusive paths)', async () => {
		const onselect = vi.fn();
		const { getAllByRole, getByPlaceholderText, getByRole } = render(ContactPicker, {
			props: { open: true, contacts: PEERS, onselect },
		});
		await fireEvent.click(getAllByRole('option')[0]); // Alice selected
		// Typing in the manual field clears the row selection; confirm now emits the typed value.
		await fireEvent.input(getByPlaceholderText(/npub1/i), { target: { value: 'npub1typed' } });
		await fireEvent.click(getByRole('button', { name: /select/i }));
		expect(onselect).toHaveBeenCalledTimes(1);
		expect(onselect.mock.calls[0][0]).toBe('npub1typed');
	});
});

describe('ContactPicker (M21 W3) — empty contact list', () => {
	it('renders a sane message, not a blank box, when there are no contacts', () => {
		const { getByText, getByPlaceholderText } = render(ContactPicker, {
			props: { open: true, contacts: [] },
		});
		// The empty state points the user at the manual field rather than vanishing.
		expect(getByText(/don.t have any contacts yet/i)).toBeTruthy();
		// The manual npub field is still present and usable.
		expect(getByPlaceholderText(/npub1/i)).toBeTruthy();
	});
});

// ── Wiring coverage (source-scan, the repo's route-page guard idiom — see chat-w3/w4, topics-w9) ──
// Both sites mount <ContactPicker> and route its onselect npub into the EXISTING command path:
// Topics → topicInvite(topicId, npub), Chat → setting composeTo (which reuses the same validation +
// send path as typing). No second send route was introduced.

describe('ContactPicker wiring — Topics → Invite', () => {
	it('mounts ContactPicker and routes onselect through topicInvite (one npub)', () => {
		const src = topicsSrc();
		expect(src).toContain("from '$lib/components/ContactPicker.svelte'");
		// The picker's onselect handler calls topicInvite — the same command the old inline field used.
		const fnOpen = src.indexOf('async function inviteChosen');
		expect(fnOpen).toBeGreaterThan(-1);
		const fnClose = src.indexOf('\n\t}', fnOpen);
		const region = src.slice(fnOpen, fnClose === -1 ? src.length : fnClose);
		expect(region).toContain('topicInvite');
		// The picker is mounted with the contacts store + the Invite CTA.
		expect(src).toContain('title="Invite to Topic"');
		expect(src).toContain('confirmLabel="Invite"');
		expect(src).toMatch(/contacts=\{\$contacts\}/);
	});
});

describe('ContactPicker wiring — Chat → compose "+"', () => {
	it('mounts ContactPicker and routes onselect into composeTo (same validation path as typing)', () => {
		const src = chatSrc();
		expect(src).toContain("from '$lib/components/ContactPicker.svelte'");
		// The compose modal's recipient field + a "Contacts" affordance both feed composeTo.
		expect(src).toContain('class="recipient-row"');
		expect(src).toContain('composePickerOpen = true');
		// Selecting a contact sets composeTo — the SAME state the free-text field binds to, so the
		// existing isComposeToSelf/composeRecipientKind validation + handleComposeSend apply unchanged.
		expect(src).toMatch(/onselect=\{\(npub\) => \{ composeTo = npub/);
		// The free-text recipient entry is NOT removed (owner: "in addition to its current form").
		expect(src).toContain('placeholder="npub or hbk share code…"');
	});
});
