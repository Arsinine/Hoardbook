// M21 W4 — the fixed word→hue CSS table for the coloured fingerprint. Pure rendering helper.
//
// `CachedPeer.fingerprint` carries three words selected by Rust (`hb_core::fingerprint`), each one of
// the 16 words below, plus a single `colorHex` swatch. The swatch tints the avatar ring (behaviour 4);
// the WORDS themselves are materials and colours ("amber", "cedar", "garnet"…), so giving each a fixed
// hue is *rendering an existing value* — it does not re-derive the algorithm (M3 decision #7: the Rust
// side picks the word; this side only colours it for the at-a-glance scan).
//
// An unknown word (a future Rust `WORDS` change the UI hasn't caught up with) falls back to the default
// text colour — never a crash, never unstyled-and-invisible.

/** The 16 fingerprint words IN ORDER, matching `crates/hb-core/src/fingerprint.rs::WORDS` verbatim.
 *  Exposed for the test that pins the keys to the Rust source of truth. */
export const FINGERPRINT_WORDS: readonly string[] = [
	'amber', 'basalt', 'cedar', 'delta', 'ember', 'fjord', 'garnet', 'harbor',
	'indigo', 'jade', 'kelp', 'lumen', 'marble', 'nimbus', 'onyx', 'pewter',
];

/** Fixed word→hue CSS table. Each word gets a stable, visually-distinct oklch colour. */
const WORD_HUES: Record<string, string> = {
	amber:  'oklch(0.80 0.15 75)',
	basalt: 'oklch(0.66 0.03 255)',
	cedar:  'oklch(0.68 0.11 50)',
	delta:  'oklch(0.76 0.11 200)',
	ember:  'oklch(0.71 0.17 35)',
	fjord:  'oklch(0.70 0.11 230)',
	garnet: 'oklch(0.64 0.17 15)',
	harbor: 'oklch(0.68 0.09 245)',
	indigo: 'oklch(0.66 0.16 285)',
	jade:   'oklch(0.76 0.13 165)',
	kelp:   'oklch(0.68 0.12 140)',
	lumen:  'oklch(0.90 0.10 100)',
	marble: 'oklch(0.88 0.02 260)',
	nimbus: 'oklch(0.76 0.04 250)',
	onyx:   'oklch(0.60 0.035 285)',
	pewter: 'oklch(0.72 0.02 265)',
};

/** Map a fingerprint word to its fixed CSS colour, or `null` for an unknown word (caller falls back to
 *  the default text colour — the card never crashes or renders an invisible word). */
export function fingerprintWordColor(word: string): string | null {
	return WORD_HUES[word] ?? null;
}
