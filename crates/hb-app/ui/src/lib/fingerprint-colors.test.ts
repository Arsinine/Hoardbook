// M21 W4 — unit tests for the fixed word→hue CSS table. Pure helper, DOM-free.
import { describe, it, expect } from 'vitest';
import { fingerprintWordColor, FINGERPRINT_WORDS } from './fingerprint-colors.js';

describe('fingerprint-colour table — the 16 Rust words map, unknown falls back', () => {
	it('all 16 words map to a non-null oklch colour', () => {
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

	it('the 16 keys equal Rust WORDS in order (fingerprint.rs source of truth)', () => {
		expect(FINGERPRINT_WORDS).toEqual([
			'amber', 'basalt', 'cedar', 'delta', 'ember', 'fjord', 'garnet', 'harbor',
			'indigo', 'jade', 'kelp', 'lumen', 'marble', 'nimbus', 'onyx', 'pewter',
		]);
	});

	it('the three golden-vector words each map (jade, fjord, garnet, onyx, harbor)', () => {
		// These are the words the fingerprint_vectors.json fixtures carry — make sure none slipped
		// through the table when renaming.
		expect(fingerprintWordColor('jade')).not.toBeNull();
		expect(fingerprintWordColor('fjord')).not.toBeNull();
		expect(fingerprintWordColor('garnet')).not.toBeNull();
		expect(fingerprintWordColor('onyx')).not.toBeNull();
		expect(fingerprintWordColor('harbor')).not.toBeNull();
	});
});
