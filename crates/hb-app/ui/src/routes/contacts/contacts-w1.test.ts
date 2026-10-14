// M17 W1 — "Message" everywhere. Source-text assertions for the contacts page (the repo's
// established pattern for route-page guards is source scanning — see mas-inv5-no-download.test.ts.
// The contacts page's onMount fan-out across many api calls and `$app/navigation`'s `goto` makes a
// full mount heavier than the affordance check warrants, so we pin the user-facing wiring the way
// the existing INV guards do.)
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const contactsSrc = () =>
	readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');

describe('Contacts page — M17 W1 Message affordances', () => {
	it('collapsed_row_exposes_a_Message_action_beside_Browse', () => {
		// A "Message" control exists on the collapsed contact row (the primary verb becomes visible),
		// reaching the guarded `/chat?peer=<npub>` deep-link in ≤1 click (no menu open required).
		const src = contactsSrc();
		expect(src).toContain('>Message<');
		expect(src).toContain("goto('/chat?peer=' + peer.npub)");
	});

	it('overflow_menu_contains_a_Message_item', () => {
		// The ⋯ overflow menu (Refresh / Edit groups… / Edit tags… / Remove) gains the primary verb.
		// The menu block sits inside <OverflowMenu ...>...</OverflowMenu>; assert a Message menu-item
		// button exists there with the same peer deep-link target.
		const src = contactsSrc();
		const menuOpen = src.indexOf('<OverflowMenu');
		expect(menuOpen).toBeGreaterThan(-1);
		const menuClose = src.indexOf('</OverflowMenu>', menuOpen);
		expect(menuClose).toBeGreaterThan(menuOpen);
		const menuBlock = src.slice(menuOpen, menuClose);
		expect(menuBlock).toContain('>Message<');
		expect(menuBlock).toContain("goto('/chat?peer=' + peer.npub)");
	});

	it('Message_menu_item_closes_the_menu_after_navigation', () => {
		// Follows the existing menu-item idiom (each item sets menuOpenFor = null on click).
		const src = contactsSrc();
		const menuOpen = src.indexOf('<OverflowMenu');
		const menuClose = src.indexOf('</OverflowMenu>', menuOpen);
		const menuBlock = src.slice(menuOpen, menuClose);
		expect(menuBlock).toMatch(/menuOpenFor\s*=\s*null/);
	});

	// QURATOR-368b: the former `dblclick_guard_still_ignores_inner_controls_including_the_new_
	// Message_affordance` source-scan (pinning `closest('button, a')` in ondblclick) was replaced
	// by its MOUNTED equivalent — contacts-q342-compact-list.test.ts, 'inner controls and modifier
	// clicks never select or navigate (iv)', which drives BOTH click and dblclick through the real
	// ⋯ control against the rendered page. Coverage moved, not deleted.
});
