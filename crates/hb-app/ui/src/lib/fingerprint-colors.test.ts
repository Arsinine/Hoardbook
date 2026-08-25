// M21 W4 — unit tests for the fixed word→hue CSS table. Pure helper, DOM-free.
// QURATOR-121 #24 widened the table 16→128 words alongside the Rust derivation's 3→5 words.
import { describe, it, expect } from 'vitest';
import { fingerprintWordColor, FINGERPRINT_WORDS } from './fingerprint-colors.js';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));

describe('fingerprint-colour table — the 128 Rust words map, unknown falls back', () => {
	it('all 128 words map to a non-null oklch colour', () => {
		for (const w of FINGERPRINT_WORDS) {
			const c = fingerprintWordColor(w);
			expect(c, `${w} must map to a colour`).not.toBeNull();
			expect(c).toMatch(/^oklch\(/);
		}
	});

	it('an unknown word falls back to null (no crash, no invisible-and-unstyled word)', () => {
		expect(fingerprintWordColor('mystery')).toBeNull();
		expect(fingerprintWordColor('')).toBeNull();
		// A case mismatch is unknown — the Rust side emits lowercase.
		expect(fingerprintWordColor('Amber')).toBeNull();
	});

	it('the 128 keys equal Rust WORDS in order (fingerprint.rs source of truth)', () => {
		// Source-of-truth extraction: slice the WORDS array out of the Rust file so this cannot
		// drift from a hand-copied list (the 16→128 widening is exactly when a hand copy would).
		const rust = readFileSync(resolve(here, '../../../../hb-core/src/fingerprint.rs'), 'utf8');
		const m = rust.match(/const WORDS: \[&str; \d+\] = \[(.*?)\];/s);
		expect(m, 'the Rust WORDS array must be found').not.toBeNull();
		const rustWords = [...m![1].matchAll(/"([a-z]+)"/g)].map((x) => x[1]);
		expect(rustWords.length, 'the Rust list widened again?').toBe(128);
		expect(FINGERPRINT_WORDS).toEqual(rustWords);
	});

	it('the golden-vector words each map (thorn, jetty, luster, trellis, nacre, larch, tulip, citrine)', () => {
		// These are the words the fingerprint_vectors.json fixtures carry — make sure none slipped
		// through the table when regenerating.
		for (const w of ['thorn', 'jetty', 'luster', 'trellis', 'nacre', 'larch', 'tulip', 'citrine', 'beacon', 'glacier']) {
			expect(fingerprintWordColor(w), `${w} must map`).not.toBeNull();
		}
	});
});
