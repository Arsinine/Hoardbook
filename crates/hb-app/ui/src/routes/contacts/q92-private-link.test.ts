// QURATOR-92 (Contacts half) — the card-detail "Private collections" section rendered the peer's
// sealed collections as an INERT panel: the owner could see they exist but had no way to open
// them where collections are actually browsed. This pins the fix: the section header is now a
// real link to `/browse?peer=<npub>` (the same deep-link the row's "Browse" button uses), while
// the CollectionPanel detail stays (contacts-w4-card.test.ts pins that it still lives in the
// detail region).
//
// Source-scan rather than mount: the assertion is about the markup shape of one region, and the
// pin is the href + its peer param. The mount-side behaviour (the section renders at all) is
// already pinned by contacts-w4-card.test.ts; this file pins the LINK.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const src = readFileSync(fileURLToPath(new URL('./+page.svelte', import.meta.url)), 'utf8');

/** The Private-collections block of the card detail, sliced out so absence assertions are scoped. */
function privateSection(s: string): string {
	const start = s.indexOf('class="private-section"');
	if (start === -1) return '';
	const end = s.indexOf('not-drm-note', start);
	return s.slice(start, end === -1 ? undefined : end + 200);
}

describe('QURATOR-92 — the Contacts private-collections section links into Browse', () => {
	it('the section header is a real link to /browse?peer=<npub>', () => {
		const section = privateSection(src);
		expect(section).not.toBe('');
		// Svelte-interpolating href forms ONLY (verified against the project's own compiler):
		//   href="/browse?peer={peer.npub}"          -> `/browse?peer=${peer.npub ?? ''}` (interpolates)
		//   href={`/browse?peer=${peer.npub}`}       -> template literal (interpolates)
		// The predecessor regex only accepted braced-quoted forms — and `href={'/browse?peer={peer.npub}'}`
		// compiles to the LITERAL string '/browse?peer={peer.npub}' ({peer.npub} is not interpolated
		// inside a quoted JS string), so that regex could only ever have been satisfied by a dead link.
		// It is tightened here, not loosened: it now rejects that literal-form false positive.
		expect(section).toMatch(/href=("\/browse\?peer=\{peer\.npub\}"|\{`\/browse\?peer=\$\{peer\.npub\}`\})/);
	});

	it('the link is anchored on the "Private collections" label (same words the inert row had)', () => {
		const section = privateSection(src);
		expect(section).toMatch(/Private collections/);
	});

	it('the CollectionPanel detail still renders below the link (w4-card keeps its pin)', () => {
		const section = privateSection(src);
		expect(section).toMatch(/<CollectionPanel collection=\{col\} \/>/);
	});
});
