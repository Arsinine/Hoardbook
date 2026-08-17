// QURATOR-103 — pin WCAG AA contrast for the --fg-dim token.
//
// --fg-dim is used at 10.5–11.5px (placeholder text, hint/footer copy, small button
// labels), which is deep below the 18px large-text threshold, so AA requires 4.5:1.
// The ratio is computed from the oklch literals read OUT OF src/app.css itself, so this
// test tracks the real stylesheet, not a copy.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join, dirname } from 'node:path';

const APP_CSS = join(dirname(fileURLToPath(import.meta.url)), '..', 'app.css');

/** Pull the `--<name>: oklch(L C H)` declaration out of the real stylesheet.
 *  Returns [L, C, H]. Throws if the token is missing or the format drifted — a
 *  silent "0 declarations found" must never read as a pass. */
function tokenFromCss(name: string): [number, number, number] {
	const css = readFileSync(APP_CSS, 'utf8');
	const re = new RegExp(`--${name}\\s*:\\s*oklch\\(\\s*([\\d.]+)\\s+([\\d.]+)\\s+([\\d.]+)\\s*\\)`);
	const m = re.exec(css);
	if (!m) {
		throw new Error(
			`--${name}: no oklch(L C H) declaration found in ${APP_CSS} — ` +
				`token missing, renamed, or reformatted (e.g. gained an alpha). Fix the test, don't delete it.`
		);
	}
	return [parseFloat(m[1]), parseFloat(m[2]), parseFloat(m[3])];
}

/** oklch → linear-light sRGB via standard OKLab math (Björn Ottosson), gamut-clamped. */
function oklchToSrgbLinear(L: number, C: number, hueDeg: number): [number, number, number] {
	const h = (hueDeg * Math.PI) / 180;
	const a = C * Math.cos(h);
	const b = C * Math.sin(h);
	const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
	const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
	const s_ = L - 0.0894841775 * a - 1.291485548 * b;
	const l = l_ ** 3;
	const m = m_ ** 3;
	const s = s_ ** 3;
	const clamp = (v: number) => Math.min(1, Math.max(0, v));
	return [
		clamp(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
		clamp(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
		clamp(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
	];
}

/** WCAG 2.x relative luminance from linear sRGB (pre-encoded channel values). */
function relativeLuminance([r, g, b]: [number, number, number]): number {
	return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrastRatio(a: [number, number, number], b: [number, number, number]): number {
	const la = relativeLuminance(a);
	const lb = relativeLuminance(b);
	const lighter = Math.max(la, lb);
	const darker = Math.min(la, lb);
	return (lighter + 0.05) / (darker + 0.05);
}

/** Diagnostic label: linear-light → gamma-encoded sRGB bytes (what the eye sees). */
const fmt = ([r, g, b]: [number, number, number]) => {
	const enc = (c: number) =>
		c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
	return `rgb(${[r, g, b].map((c) => Math.round(enc(c) * 255)).join(' ')})`;
};

describe('QURATOR-103: --fg-dim meets WCAG AA (4.5:1) for small text', () => {
	it('--fg-dim and --bg declarations exist in app.css (guard against vacuous pass)', () => {
		// tokenFromCss throws if either regex fails, so merely resolving them is the pin.
		expect(tokenFromCss('fg-dim')).toBeDefined();
		expect(tokenFromCss('bg')).toBeDefined();
		expect(tokenFromCss('bg-input')).toBeDefined();
		expect(tokenFromCss('fg-muted')).toBeDefined();
	});

	it('--fg-dim ≥ 4.5:1 against --bg', () => {
		const fg = oklchToSrgbLinear(...tokenFromCss('fg-dim'));
		const bg = oklchToSrgbLinear(...tokenFromCss('bg'));
		const ratio = contrastRatio(fg, bg);
		expect(ratio, `--fg-dim ${fmt(fg)} on --bg ${fmt(bg)} = ${ratio.toFixed(3)}:1`).toBeGreaterThanOrEqual(4.5);
	});

	it('--fg-dim ≥ 4.5:1 against --bg-input (the .hb-input::placeholder backdrop, app.css .hb-input background)', () => {
		const fg = oklchToSrgbLinear(...tokenFromCss('fg-dim'));
		const bgi = oklchToSrgbLinear(...tokenFromCss('bg-input'));
		const ratio = contrastRatio(fg, bgi);
		expect(ratio, `--fg-dim ${fmt(fg)} on --bg-input ${fmt(bgi)} = ${ratio.toFixed(3)}:1`).toBeGreaterThanOrEqual(4.5);
	});

	it('--fg-dim stays visually dimmer than --fg-muted (lightness strictly below)', () => {
		const dimL = tokenFromCss('fg-dim')[0];
		const mutedL = tokenFromCss('fg-muted')[0];
		expect(dimL, `--fg-dim L=${dimL} must stay below --fg-muted L=${mutedL}`).toBeLessThan(mutedL);
	});
});
