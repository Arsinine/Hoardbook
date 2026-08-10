// QURATOR-52 item 4 — "Google Fonts CDN links remain in app.html (M14) — a privacy and offline-use
// wart. Fonts should be self-hosted."
//
// The defect: `app.html` shipped `<link rel="preconnect">` + `<link href="https://fonts.googleapis…">`
// pointing at Google's CDN. Every app launch therefore made a third-party request (IP + launch
// timestamp leak) and the UI fell back to system fonts when offline — in an app explicitly designed
// to work offline. The fix vendors Inter + JetBrains Mono as woff2 into static/fonts/ and declares
// them with local @font-face rules (see app.css).
//
// This guard scans at the SOURCE (the repo's established pattern — see mas-inv5-no-download,
// input-contract, copy-audit): the rendered head is a function of app.html + the CSS, so we pin the
// only thing that can reintroduce the defect — a `fonts.googleapis.com` / `fonts.gstatic.com`
// reference in either file, or an @font-face src that is not local.
//
// Sentinel-collision discipline (repo rule): strip comments BEFORE asserting absence. The page and
// the CSS legitimately document the ruling ("no Google Fonts CDN request"), so a naive whole-file
// scan would red on its own documentation. We strip `/* */` and `<!-- -->` comments first, then scan
// what remains — the thing that must never appear is the *reference*, not the word.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const APP_HTML = new URL('../app.html', import.meta.url);
const APP_CSS = new URL('../app.css', import.meta.url);

/** Strip `/* … *​/` and `<!-- … -->` comments so the scan hits only live markup/CSS. */
function stripComments(source: string): string {
	return source
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/<!--[\s\S]*?-->/g, '');
}

const EXTERNAL_FONT_ORIGINS = ['fonts.googleapis.com', 'fonts.gstatic.com'];

/** Every @font-face src URL in the CSS, in order. */
function fontFaceSrcs(css: string): string[] {
	return [...css.matchAll(/@font-face\s*{[\s\S]*?src:\s*url\(([^)]+)\)/g)].map((m) => m[1].replace(/['"]/g, ''));
}

describe('QURATOR-52 — no external font origin (self-hosted fonts)', () => {
	const html = stripComments(readFileSync(APP_HTML, 'utf8'));
	const css = stripComments(readFileSync(APP_CSS, 'utf8'));

	it('app.html has no Google Fonts CDN link, preconnect, or stylesheet', () => {
		for (const origin of EXTERNAL_FONT_ORIGINS) {
			expect(html, `app.html must not reference ${origin}`).not.toContain(origin);
		}
		// Positive assertion too: the local @font-face rules live in app.css (imported by +layout),
		// so app.html needs NO font stylesheet link at all — the defect was a <link> in the head.
		expect(html).not.toContain('rel="stylesheet"');
		expect(html).not.toContain('rel="preconnect"');
	});

	it('the CSS has no Google Fonts CDN origin', () => {
		for (const origin of EXTERNAL_FONT_ORIGINS) {
			expect(css, `app.css must not reference ${origin}`).not.toContain(origin);
		}
	});

	it('every @font-face src is a local /fonts/ path that resolves to a file on disk', () => {
		const srcs = fontFaceSrcs(css);
		expect(srcs.length).toBeGreaterThan(0);
		for (const src of srcs) {
			expect(src, '@font-face src must be local under /fonts/').toMatch(/^\/fonts\/[a-z0-9.-]+\.woff2$/);
			const file = fileURLToPath(new URL(`../../static${src}`, import.meta.url));
			expect(() => readFileSync(file), `font file ${src} must exist and be non-empty`).not.toThrow();
			expect(readFileSync(file).length).toBeGreaterThan(0);
		}
	});

	it('both families referenced by the design tokens have at least one local face', () => {
		const srcs = fontFaceSrcs(css).join(' ');
		expect(srcs).toContain('/fonts/inter-latin.woff2');
		expect(srcs).toContain('/fonts/jetbrains-mono-latin.woff2');
	});
});
