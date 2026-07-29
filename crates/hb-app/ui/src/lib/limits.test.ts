import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { MAX_DESCRIPTION_CHARS, MAX_TAGS, MAX_TAG_CHARS } from './limits.js';

// The UI shows these limits; the backend enforces them. If the two drift, the user is told one
// number and clamped to another — which looks exactly like the app eating their text. Parse the
// Rust source rather than duplicating the values in a fixture.
const RUST = readFileSync(resolve(__dirname, '../../../../hb-core/src/types.rs'), 'utf8');

function rustConst(name: string): number {
	const m = RUST.match(new RegExp(`pub const ${name}: usize = (\\d+);`));
	if (!m) throw new Error(`${name} not found in hb-core/src/types.rs — was it renamed?`);
	return Number(m[1]);
}

describe('metadata ceilings mirror hb-core', () => {
	it('matches the Rust source of truth', () => {
		expect(MAX_DESCRIPTION_CHARS).toBe(rustConst('MAX_DESCRIPTION_CHARS'));
		expect(MAX_TAGS).toBe(rustConst('MAX_TAGS'));
		expect(MAX_TAG_CHARS).toBe(rustConst('MAX_TAG_CHARS'));
	});

	it('keeps the description at a tweet-ish length', () => {
		// Owner ruling 2026-07-29: 255, "enough for a tweet". Pinned so a later budget change has
		// to restate the product intent rather than quietly widening it.
		expect(MAX_DESCRIPTION_CHARS).toBe(255);
	});
});
