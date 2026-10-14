// @vitest-environment jsdom
// QURATOR-361 (HB 351·F2): AddContactPanel keeps ONLY the lookup-by-ID card — the §6 Discover
// section (hit cards, chips, autocomplete, tag cloud, pager, capped notice) moved to DiscoverBook
// (QURATOR-351 r5). This file used to pin that section (M17 W1 / M20 W3 / QURATOR-70 / QURATOR-104
// suites, all Discover-scoped); now it pins its ABSENCE, plus the lookup card staying renderable.
// The lookup card's behaviour tests live in contacts-w2.test.ts (they scan the page+panel source).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';
import AddContactPanel from './AddContactPanel.svelte';
import { contacts } from '../stores.js';

vi.mock('../api.js', () => ({
	pasteKey: vi.fn(),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	contacts.set([]);
});

describe('AddContactPanel — lookup-only panel after the Discover move (QURATOR-361 F2)', () => {
	// MUTATION PROOF (P-10, seen red 2026-10-14): re-insert a discover node in the template —
	// add `\t\t\t\t<div class="discover-section">Discover</div>` immediately after the
	// `</div>` that closes .lookup-section (template line 51 post-edit), see this file RED,
	// remove the line, touch, GREEN. The test asserts both the discover classes and the copy.
	it('renders the lookup input and NO discover section, chips, or tag affordance', async () => {
		const { container } = render(AddContactPanel, { props: { open: true } });
		await tick();
		// The lookup input still renders (the panel's remaining job).
		expect(container.querySelector('input.search-input')).toBeTruthy();
		// No element carrying a discover class survives (discover-* and disc-* families).
		expect(container.querySelectorAll('[class*="discover"], [class*="disc-"]').length).toBe(0);
		// No "Discover" copy and no bare "tag" affordance text anywhere in the rendered panel.
		const text = container.textContent ?? '';
		expect(/discover/i.test(text)).toBe(false);
		expect(/(^|\W)tag(\W|$)/i.test(text)).toBe(false);
	});
});
