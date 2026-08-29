// M17 W9 — Topics uses the shared app shell. Source-text assertions, the repo's established
// pattern for route-page guards (see contacts-w1.test.ts): the topics page's onMount fan-out
// makes a full mount heavier than a chrome check warrants.
//
// W9 is PAINT. The behaviour guard is that every topics-view test passes unmodified; this file
// pins only the shell markup and the de-duplicated card recipe, so a later restyle cannot
// silently re-fork Topics from the rest of the app.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const topicsSrc = () => readFileSync(new URL('./+page.svelte', import.meta.url), 'utf8');
const contactsSrc = () =>
	readFileSync(new URL('../contacts/+page.svelte', import.meta.url), 'utf8');

describe('Topics page — M17 W9 shared shell', () => {
	// The opening tag is matched as a PATTERN, not an exact string: QURATOR-81 added
	// `data-tauri-drag-region` to these topbars (the topbar is the window's drag handle), and an
	// exact-string assertion cannot survive any attribute. `class="topbar"` is still required
	// verbatim, so a rename to `.topbar-x` or a bespoke header still fails — the guarantee this test
	// exists for is unchanged.
	const TOPBAR_OPEN = /<div class="topbar"[^>]*>/;

	it('opens_with_the_shared_topbar_not_a_bespoke_header', () => {
		const src = topicsSrc();
		expect(src).toMatch(TOPBAR_OPEN);
		expect(src).toContain('<div class="topbar-title">Topics</div>');
		expect(src).toContain('class="topbar-sub"');
		// The one-off header the owner saw as "a different app" is gone.
		expect(src).not.toContain('<header>');
		expect(src).not.toContain('<h1>');
	});

	it('topbar_markup_matches_the_contacts_shell_shape', () => {
		// Same three-part structure Contacts uses, so the two read as one app.
		const topics = topicsSrc();
		const contacts = contactsSrc();
		// Both files must open a `.topbar` (pattern, per the note above) …
		expect(contacts).toMatch(TOPBAR_OPEN);
		expect(topics).toMatch(TOPBAR_OPEN);
		// … and carry the same two inner parts, which are still exact strings.
		for (const marker of ['class="topbar-title"', 'class="topbar-sub"']) {
			expect(contacts).toContain(marker);
			expect(topics).toContain(marker);
		}
		// Title ramp is the shared 17px/600, not the old bespoke 18px/700.
		expect(topics).toContain('.topbar-title { font-size: 17px; font-weight: 600;');
		expect(topics).not.toMatch(/header h1/);
	});

	it('page_actions_live_in_topbar_actions', () => {
		const src = topicsSrc();
		const open = src.indexOf('<div class="topbar-actions">');
		expect(open).toBeGreaterThan(-1);
		const block = src.slice(open, src.indexOf('</div>\n</div>', open));
		// QURATOR-144 W2 removed the joined/directory tab split (ONE merged tree), so the `.tabs`
		// block this test used to demand is gone by design. What must survive: the page's primary
		// action lives in the shared topbar actions slot, and the tree (not a tab) is what fills
		// the list pane below it.
		expect(block).toContain('+ New Topic');
		expect(src).toContain('<section class="master-detail">');
		expect(src).toContain('class="root-header"');
		expect(src).not.toContain('class="tabs"'); // the retired tab split must not creep back
		// The old local wrapper is retired.
		expect(src).not.toContain('header-actions');
	});

	it('the_scroll_body_replaces_the_local_page_wrapper', () => {
		const src = topicsSrc();
		expect(src).toContain('<div class="body">');
		expect(src).not.toContain('<div class="page">');
		expect(src).not.toMatch(/^\t\.page \{/m);
	});
});

describe('Topics page — flat layout, matching Browse/Chat (2026-08-15)', () => {
	// The surface audit found Topics was the only master-detail page boxed like Settings/Contacts
	// instead of flat like its actual layout siblings (Browse's .left-panel, Chat's .convo-sidebar —
	// both a divider, no card). Stripped; this pins the replacement, not a re-fork of the old box.
	it('no pane carries the old .surface card treatment', () => {
		const src = topicsSrc();
		expect(src).not.toContain('class="surface');
		expect(src).not.toMatch(/\.surface \{/);
		// The three-copy regression the old test guarded against is still impossible: there is no
		// card recipe left to duplicate.
		const bgDecls = src.match(/background: var\(--bg-elev1\); border: 1px solid var\(--border\); border-radius: 10px/g);
		expect(bgDecls).toBeNull();
	});

	it('the list pane uses a divider, not a box — same recipe as Browse/Chat', () => {
		const src = topicsSrc();
		expect(src).toMatch(/\.list-pane \{[^}]*border-right: 1px solid var\(--border\)/);
		// No visible gap between the divider and the detail pane (Browse/Chat are gap: 0 too).
		expect(src).toMatch(/\.master-detail \{[^}]*gap: 0/);
	});

	it('section_label_is_on_the_shared_type_ramp', () => {
		// contacts/settings both use 10.5px / letter-spacing 1.2px / weight 600.
		const src = topicsSrc();
		expect(src).toMatch(/\.section-label \{[^}]*font-size: 10\.5px/);
		expect(src).toMatch(/\.section-label \{[^}]*letter-spacing: 1\.2px/);
		expect(src).toMatch(/\.section-label \{[^}]*font-weight: 600/);
	});
});
