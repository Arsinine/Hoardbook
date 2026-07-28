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
	it('opens_with_the_shared_topbar_not_a_bespoke_header', () => {
		const src = topicsSrc();
		expect(src).toContain('<div class="topbar">');
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
		for (const marker of ['<div class="topbar">', 'class="topbar-title"', 'class="topbar-sub"']) {
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
		expect(block).toContain('class="tabs"');
		expect(block).toContain('+ New Topic');
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

describe('Topics page — M17 W9 card recipe', () => {
	it('the_three_panes_share_one_surface_declaration', () => {
		const src = topicsSrc();
		for (const pane of ['list-pane', 'detail-pane', 'discover-tab']) {
			expect(src).toContain(`surface ${pane}`);
		}
		// Exactly one place declares the card recipe; the three copies are gone.
		const bgDecls = src.match(/background: var\(--bg-elev1\); border: 1px solid var\(--border\); border-radius: 10px/g);
		expect(bgDecls).toBeNull();
		expect(src).toMatch(/\.surface \{[^}]*background: var\(--bg-elev1\)/);
	});

	it('section_label_is_on_the_shared_type_ramp', () => {
		// contacts/settings both use 10.5px / letter-spacing 1.2px / weight 600.
		const src = topicsSrc();
		expect(src).toMatch(/\.section-label \{[^}]*font-size: 10\.5px/);
		expect(src).toMatch(/\.section-label \{[^}]*letter-spacing: 1\.2px/);
		expect(src).toMatch(/\.section-label \{[^}]*font-weight: 600/);
	});
});
