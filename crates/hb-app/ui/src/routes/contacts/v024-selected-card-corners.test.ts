// @vitest-environment jsdom
// v0.24.0 devtest item 2 — "When a contact is selected in Contacts, the bottom two corners of the
// highlighted contact spontaneously turns from rounded back to rectangles."
//
// Cause: `.contact-block.open .contact-card` squared the card's bottom corners so an INLINE detail
// could continue the card (devtest v0.12.4 #6). The detail moved to the right pane in QURATOR-360,
// but the rule stayed — and `.open` lands 250 ms after the click (the held select), so the corners
// snapped square a beat after selection.
//
// Mounted (P-4) for the class half: the selected row must NOT carry a state class that any rule
// keys a corner change on. jsdom does not inject the component's <style>, so the CSS half is a
// comment-stripped scan of the style block (P-12): no rule may zero a .contact-card corner.
// P-10 MUTATION (orchestrator): restore `class:open={detailExpanded === peer.npub}` on
// .contact-block → REDS the mount; restore the `.contact-block.open .contact-card { border-bottom-
// left-radius: 0; … }` rule → REDS the scan.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import ContactsPage from './+page.svelte';
import { contacts } from '$lib/stores.js';
import type { ContactSummary, Profile } from '$lib/types.js';

vi.mock('$lib/api.js', () => ({
	groupsGet: vi.fn().mockResolvedValue([]),
	browsePrivateCollections: vi.fn().mockResolvedValue([]),
	onlineCount: vi.fn().mockResolvedValue({ online: 0, fetched_at: new Date().toISOString(), relay_set: [], fresh: [] }),
	relayStatus: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	privateAudienceList: vi.fn().mockResolvedValue([]),
	refreshContact: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const NPUB = 'npub1v024c' + 'c'.repeat(50);

afterEach(() => {
	cleanup();
	contacts.set([]);
});

describe('v0.24.0 item 2 — a selected contact card keeps its rounded corners', () => {
	it('selecting a row adds no corner-changing state class to its block', async () => {
		contacts.set([
			{
				npub: NPUB,
				has_browse_key: false,
				collections: [],
				online: false,
				last_fetched: new Date().toISOString(),
				local_tags: [],
				profile: { display_name: 'Corner Case', bio: '', tags: [], languages: [], social_links: [], willing_to: [], updated: '2026-08-01T00:00:00Z' } as unknown as Profile,
			} as unknown as ContactSummary,
		]);
		const { container } = render(ContactsPage);
		await waitFor(() => expect(container.querySelector('.contact-card')).not.toBeNull());
		fireEvent.click(container.querySelector('.contact-card')!);
		// The held select lands after 250 ms; the pane shows the contact once it has.
		await waitFor(() => expect(container.querySelector('.pane-name')?.textContent).toBe('Corner Case'));
		const block = container.querySelector('.contact-block') as HTMLElement;
		expect(block.classList.contains('open'), 'no .open on the selected block').toBe(false);
	});

	it('no rule in the page squares a .contact-card corner', () => {
		const src = readFileSync(resolve(process.cwd(), 'src/routes/contacts/+page.svelte'), 'utf8');
		const css = src.slice(src.indexOf('<style>')).replace(/\/\*[\s\S]*?\*\//g, '');
		const rules = [...css.matchAll(/([^{}]+)\{([^}]*)\}/g)].filter(([, sel]) => sel.includes('.contact-card'));
		expect(rules.length, 'the scan found the .contact-card rules').toBeGreaterThan(0);
		for (const [, sel, body] of rules) {
			expect(body, `${sel.trim()} must not zero a corner`).not.toMatch(/border-(?:bottom|top)-(?:left|right)-radius:\s*0/);
		}
	});
});
