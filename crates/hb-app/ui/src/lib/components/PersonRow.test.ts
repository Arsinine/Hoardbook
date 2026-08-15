// @vitest-environment jsdom
// Hoardbook Topics draft r1 — the roster row (PersonRow): avatar + name + coloured 3-word
// fingerprint + presence dot. A BEHAVIOURAL test (real mount), not a source-scan — CLAUDE.md §7
// says the components mount fine, and the interesting claims here (fingerprint words render in the
// fingerprintWordColor hues, absence is graceful) are DOM claims.
//
// The absent-gracefully half is the one that matters: a roster npub is NOT guaranteed to be a saved
// contact, and even a saved CachedPeer only carries `.fingerprint` once resolved. The row must
// render avatar + name + presence with NO fingerprint line and NO crash — the M21 W4 behaviour-4
// precedent ("no ring, no word row").
import { describe, it, expect, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import PersonRow from './PersonRow.svelte';
import { fingerprintWordColor } from '../fingerprint-colors.js';

afterEach(cleanup);

const here = dirname(fileURLToPath(import.meta.url));

/** A resolved §7 fingerprint as Rust emits it (words from the fixed 16-word table + a swatch). */
const fp = { words: ['amber', 'cedar', 'jade'], colorHex: '#f00' };

/** jsdom re-serializes inline styles and trims insignificant trailing zeros ("0.80" → "0.8"), so a
 *  colour comparison must normalize both sides the same way before comparing. */
const normCss = (s: string) => s.replace(/(\d+\.\d*?)0+(?=\s|\)|$)/g, '$1');

describe('PersonRow — Hoardbook Topics draft r1 roster row', () => {
	it('renders the display name and the 3 fingerprint words when the fingerprint is available', () => {
		const { container, getByText } = render(PersonRow, {
			props: { name: 'Alice', letter: 'A', fingerprint: fp },
		});
		expect(getByText('Alice')).toBeTruthy();
		for (const w of fp.words) {
			const el = getByText(w);
			expect(el).toBeTruthy();
			// The word is COLOURED by the fixed word→hue table (rendered value only — Rust picks the
			// word; this side never derives it). Compared after the same trailing-zero normalization
			// jsdom applies when it re-serializes the attribute ("0.80" → "0.8").
			const wordColor = fingerprintWordColor(w);
			expect(wordColor, `"${w}" must be in the fixed word→hue table for this test to mean anything`).not.toBeNull();
			expect(normCss(el.getAttribute('style') ?? '')).toContain(`color: ${normCss(wordColor!)}`);
		}
		// The avatar letter is present (the letter fallback of <Avatar>).
		expect(container.textContent).toContain('A');
	});

	it('renders NO fingerprint row when the fingerprint is absent (non-contact / unresolved)', () => {
		const { container, getByText } = render(PersonRow, {
			props: { name: 'npub1abcd…wxyz', letter: 'N' },
		});
		expect(getByText('npub1abcd…wxyz')).toBeTruthy();
		// Not one fingerprint word from the fixed table renders, and no .fp-row exists at all.
		expect(container.querySelector('.fp-row')).toBeNull();
		for (const w of ['amber', 'cedar', 'jade', 'basalt']) {
			expect(container.textContent).not.toContain(w);
		}
	});

	it('renders the presence dot only when presence is KNOWN online', () => {
		// Online → dot.
		const online = render(PersonRow, { props: { name: 'Al', letter: 'A', online: true } });
		expect(online.container.querySelector('.online-dot')).not.toBeNull();
		cleanup();
		// Offline / unknown → NO dot (never a guessed state).
		const offline = render(PersonRow, { props: { name: 'Al', letter: 'A' } });
		expect(offline.container.querySelector('.online-dot')).toBeNull();
	});

	it('renders an <img> avatar for a data: picture and the letter for none', () => {
		const withPic = render(PersonRow, {
			props: { name: 'Al', letter: 'A', picture: 'data:image/webp;base64,AAAA' },
		});
		expect(withPic.container.querySelector('img')).not.toBeNull();
		cleanup();
		const noPic = render(PersonRow, { props: { name: 'Al', letter: 'A' } });
		expect(noPic.container.querySelector('img')).toBeNull();
		expect(noPic.container.textContent).toContain('A');
	});

	it('styles the online dot on the shared --online token (source wiring, jsdom computes no layout)', () => {
		// jsdom cannot compute the rendered colour; what CAN be pinned is that the dot and the row
		// are wired to the same tokens Browse's People row uses (browse/+page.svelte .online-dot),
		// so the two read as one app. Asserted on the component's own <style> block.
		const src = readFileSync(resolve(here, 'PersonRow.svelte'), 'utf8');
		const style = src.slice(src.indexOf('<style>'));
		expect(style).toMatch(/\.online-dot \{[^}]*background:\s*var\(--online\)/);
	});
});
