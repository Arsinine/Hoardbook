// QURATOR-391 devtest item 4 — the shield icon in chat's E2E notice was misaligned with the
// text "Messages are end-to-end encrypted — relays never see who sent them or what they say."
//
// Fix: the note is a flex row (icon span + text span), aligned to the FIRST LINE
// (`align-items: flex-start` + 2px icon offset) — the note's 320px max-width wraps the sentence
// to two lines at every normal width, so centering would drop the shield to the block's middle.
// Mirrors the onboarding `.ob-notice` pattern in routes/+page.svelte.
//
// WHY A SOURCE-SCAN (P-4 note, honestly stated): a full chat-page mount was judged
// disproportionate for a static-markup claim — the page's mount fan-out is far heavier than the
// claim under test needs, and the ticket explicitly blesses the declared-CSS assertion here.
// P-13 LIMIT (stated, as required): jsdom computes no layout, and even a browser only proves
// rendering at the widths sampled. This test pins the DECLARED shape (flex row, first-line
// alignment, icon in its own element); it cannot prove the pixels. The rendered geometry was
// measured in a real browser (vite dev + Playwright) and recorded in the handback.
//
// Comments are stripped before scanning (P-12) so this file's own explanation cannot satisfy it.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const src = readFileSync(resolve(process.cwd(), 'src/routes/chat/+page.svelte'), 'utf8');
// Strip comments, then take everything between the notice's own markers.
const clean = src.replace(/\/\*[\s\S]*?\*\//g, '').replace(/<!--[\s\S]*?-->/g, '');

const markupStart = clean.indexOf('<p class="privacy-note">');
const markupEnd = clean.indexOf('</p>', markupStart);
const markup = clean.slice(markupStart, markupEnd);
const css = clean.slice(clean.indexOf('<style>'));

describe('QURATOR-391 item 4 — the chat E2E notice is an aligned flex row', () => {
	it('the shield sits in its own span, the sentence in its own span (no bare inline icon)', () => {
		expect(markup).toContain('<span class="privacy-note-icon" aria-hidden="true">');
		expect(markup).toContain('{@html icons.shield}');
		expect(markup).toContain('<span class="privacy-note-text">');
		expect(markup).not.toMatch(/\{@html icons\.shield\}\s+Messages/);
	});

	it('the note is a flex row aligned to the first line, with the icon optically offset', () => {
		const rule = css.match(/\.privacy-note\s*\{([^}]*)\}/);
		expect(rule, '.privacy-note rule exists').not.toBeNull();
		expect(rule![1]).toMatch(/display:\s*flex/);
		expect(rule![1]).toMatch(/align-items:\s*flex-start/);
		const icon = css.match(/\.privacy-note-icon\s*\{([^}]*)\}/);
		expect(icon, '.privacy-note-icon rule exists').not.toBeNull();
		expect(icon![1]).toMatch(/margin-top:\s*2px/);
		expect(icon![1]).toMatch(/flex-shrink:\s*0/);
	});
});
