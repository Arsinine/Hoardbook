// M20 W4 — "Topics still does not look like the rest of the app", guarded at the source.
//
// The defect: the input contract (`.hb-input` / `.hb-textarea` / `.hb-input-mono` / `.hb-mono`) was
// re-declared locally in three route/component stylesheets, and the Topics route went further — it
// styled inputs with a BARE element selector (`input { background: var(--bg-elev2) … }`) that (1)
// used `--bg-elev2` instead of the canonical `--bg-input`, and (2) leaked onto the create-Topic
// modal fields. DESIGN_SYSTEM.md §3 records the input contract and the divergence; this guard pins
// it in code so it cannot drift back.
//
// The repo's established pattern for these guards is source scanning (see mas-inv5-no-download,
// contacts-w2, copy-audit): the rendered style is a function of too many cascade layers to assert
// at mount, so we pin the only things a route can get wrong at the source — here, that the contract
// lives in exactly one place (app.css) and nothing re-introduces the bare-element / wrong-token
// forms. The M18 W5 lesson applies: a hand-written list of violating sites cannot fix a hand-written
// list of violating sites, so the ALLOWLIST is "every stylesheet file minus the one permitted file",
// never an enumeration of the known offenders.

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
// Up from src/lib/ to src/, then up once more to the ui/ root — so `src/app.css` and
// `git ls-files "src/**/*.svelte"` (run from uiRoot) resolve against the repo's ui dir.
const uiRoot = resolve(here, '..', '..');

// Glob every Svelte/CSS stylesheet under src/, then subtract the one file permitted to declare the
// contract. This is the inverse of enumerating offenders: a NEW file that mints `.hb-input` is
// caught just as a renamed old one would be — no allowlist entry to forget to update.
function stylesheetsExcept(allowed: string[]): string[] {
	const allowedRel = new Set(allowed.map((p) => p.replace(/\\/g, '/')));
	const all = execSync('git ls-files "src/**/*.svelte" "src/**/*.css"', { cwd: uiRoot, encoding: 'utf8' })
		.split('\n')
		.map((f) => f.trim())
		.filter(Boolean)
		// Normalize to forward-slash, relative-to-uiRoot paths so the allowlist compares cleanly on
		// Windows as well as POSIX (git ls-files emits repo-relative paths with forward slashes).
		.filter((f) => !allowedRel.has(f.replace(/\\/g, '/')));
	return all.map((f) => resolve(uiRoot, f));
}

const APP_CSS = 'src/app.css';
const read = (rel: string) => readFileSync(resolve(uiRoot, rel), 'utf8');
const readAbs = (abs: string) => readFileSync(abs, 'utf8');
const rel = (abs: string) => abs.replace(uiRoot + '/', '').replace(uiRoot + '\\', '');

/** Extract `<style>` block contents from a .svelte file (the only place a Svelte component's CSS
 *  lives). app.css is read whole. Returns '' for files with no `<style>`. */
function styleBlock(source: string): string {
	const m = source.match(/<style[^>]*>([\s\S]*?)<\/style>/i);
	return m ? m[1] : '';
}

/** A CSS rule: its selector list (text before `{`) and the declaration body (text between braces).
 *  Naive — does not nest — but Svelte `<style>` blocks are flat CSS, which is all this guard scans. */
interface Rule {
	selector: string;
	body: string;
}
function rules(css: string): Rule[] {
	// Strip comments first — otherwise a `/* … */` between a selector and `{` gets captured into the
	// selector text and breaks compound matching (`/* x */ .hb-textarea` looks like two steps).
	const clean = css.replace(/\/\*[\s\S]*?\*\//g, '');
	const out: Rule[] = [];
	const re = /([^{}]+)\{([^{}]*)\}/g;
	let m: RegExpExecArray | null;
	while ((m = re.exec(clean))) {
		const selector = m[1].trim();
		// Skip at-rule preambles (@media, @keyframes, @supports) — their "selector" starts with '@'
		// and they nest braces (which the naive split mishandles). Svelte style blocks rarely need
		// them, and none of the input contract lives inside one.
		if (selector.startsWith('@')) continue;
		out.push({ selector, body: m[2] });
	}
	return out;
}

/** Split a selector list into its comma-separated components, then each component into combinator
 *  steps, and report whether ANY step targets an input-shaped surface. A step is "input-shaped" if
 *  its compound selector is the bare element `input`/`textarea`, OR carries a class whose name ends
 *  in `-input` / `-textarea` / is exactly `hb-input` etc. (covers `.hb-input`, `.search-input`,
 *  `.compose-input`, `.compose-modal-input`). Descendant selectors like `.foo input` count too. */
function targetsInput(selector: string): boolean {
	const components = selector.split(',').map((c) => c.trim());
	for (const comp of components) {
		// Split on combinators (space, >, +, ~) to catch descendant/descendant-of-input.
		const steps = comp.split(/\s*[>+~]\s*|\s+/).filter(Boolean);
		for (const step of steps) {
			if (/^(input|textarea)([:#.\[]|$)/i.test(step)) return true; // bare element (incl. pseudo/class that follows)
			// A class on the input shape: `.search-input`, `.hb-input`, `.compose-modal-input`.
			if (/\.[-\w]*(input|textarea)\b/i.test(step)) return true;
		}
	}
	return false;
}

/** Does this rule's selector have a bare `input`/`textarea` element component (no class/ancestor scope)?
 *  `input {…}`, `input, select {…}`, `input:focus {…}` are bare and catch every input in scope —
 *  the Topics defect. `.hb-input`, `.foo input` are scoped (by class or by ancestor) and are fine:
 *  Contacts' `.subheader-search input` is a legitimate descendant selector. So we flag only a
 *  TOP-LEVEL compound selector that IS the element itself (optionally with a pseudo/class trailing). */
function bareInputElement(selector: string): boolean {
	const components = selector.split(',').map((c) => c.trim());
	for (const comp of components) {
		// No combinator (no space/>/+/~) means the compound is top-level. Then it is bare if it
		// begins with the `input`/`textarea` element — NOT `.input` (a class) or `#input` (an id).
		if (/\s/.test(comp)) continue; // a descendant selector like `.foo input` is scoped by ancestor
		if (/^(input|textarea)\b/i.test(comp) && !comp.startsWith('.') && !comp.startsWith('#')) {
			return true;
		}
	}
	return false;
}

function setsBackground(body: string): boolean {
	return /(^|[;{\s])background(?:-color)?\s*:/i.test(body);
}
function bgIsElev2(body: string): boolean {
	return /background(?:-color)?\s*:\s*[^;}]*var\(--bg-elev2\)/i.test(body);
}

/** The contract classes — the appearance contract that must live only in app.css. A route may scope
 *  LAYOUT to an input via a descendant selector (`.relay-add-row > input { flex: 1 }`), but it must
 *  not re-declare the class itself (`.hb-input { background: … }`) — that is the drift W4 removes. */
const CONTRACT_CLASSES = ['.hb-input', '.hb-textarea', '.hb-input-mono', '.hb-mono'];

/** Does a rule re-declare a contract class as a TOP-LEVEL compound selector? We split the selector
 *  list on commas, then each component on combinators (space/>/+/~); the FIRST compound of a
 *  component is its "subject". A contract class as the subject (`.hb-input { … }`, `.hb-input, .x {…}`)
 *  is a re-declaration; the same class as a DESCENDANT (`.field .hb-input`) or with an ancestor
 *  (`.relay-add-row > input`) is a layout override and is permitted. This mirrors how Svelte's
 *  scoping treats a top-level class selector as global-once-promoted. */
function redeclaresContract(selector: string): string[] {
	const hits: string[] = [];
	const components = selector.split(',').map((c) => c.trim());
	for (const comp of components) {
		// The subject is the last compound in a combinator chain (`.a .b` → subject is `.b`); but a
		// contract re-declaration is the class ALONE as the whole compound (`.hb-input`), possibly
		// with a pseudo (`:focus`, `::placeholder`). Split off ancestors and inspect the final step.
		const steps = comp.split(/\s*[>+~]\s*/);
		// A single step means no CHILD/SIBLING combinator (`>`, `+`, `~`) — that is all this regex
		// splits on. It does NOT split on descendant whitespace, so `.field .hb-input` is ONE step,
		// not two. Descendant overrides are excluded by the compound check below instead: the
		// compound `.field .hb-input` neither equals `.hb-input` nor starts with `.hb-input:`/`::`,
		// so it is never counted as a re-declaration. (QURATOR-52 §6: the previous comment claimed
		// the step count did that work. It does not — the anchor below does.)
		if (steps.length > 1) continue;
		const compound = steps[0];
		for (const cls of CONTRACT_CLASSES) {
			// The compound is (or starts with) the contract class, e.g. `.hb-input` / `.hb-input:focus`.
			if (compound === cls || compound.startsWith(cls + ':') || compound.startsWith(cls + '::')) {
				hits.push(cls);
			}
		}
	}
	return hits;
}

describe('input contract W4 — declared once, in app.css', () => {
	it('app.css declares every contract class (the single source of truth)', () => {
		// Read app.css whole (no <style> wrapper) and confirm each contract class is re-declared
		// nowhere but HERE — so it must be present here. Uses the same redeclaresContract() so the
		// "what counts as a declaration" definition is identical between the positive and negative
		// assertions (no two regexes that can drift apart).
		const declared = new Set<string>();
		for (const r of rules(read(APP_CSS))) {
			for (const cls of redeclaresContract(r.selector)) declared.add(cls);
		}
		for (const cls of CONTRACT_CLASSES) {
			expect(declared, `app.css must declare ${cls}`).toContain(cls);
		}
	});

	it('no stylesheet other than app.css declares a contract class', () => {
		const offenders = stylesheetsExcept([APP_CSS]);
		const violations: string[] = [];
		for (const abs of offenders) {
			for (const r of rules(styleBlock(readAbs(abs)))) {
				for (const cls of redeclaresContract(r.selector)) {
					violations.push(`${rel(abs)} re-declares ${cls}`);
				}
			}
		}
		expect(violations, `contract classes must live only in ${APP_CSS}`).toEqual([]);
	});
});

describe('input contract W4 — no bare-element input/textarea backgrounds, no --bg-elev2 on inputs', () => {
	// The Topics defect had two halves: a BARE `input { … }` element selector (catches every input
	// in scope, including modal fields), and the wrong token (`--bg-elev2` instead of `--bg-input`).
	// Both must stay forbidden everywhere — a route that legitimately needs a one-off input look
	// must scope it to a class, not a bare element selector, and must not reach for the elev2 token
	// to do it. `--bg-elev2` remains the correct fill for cards/surfaces/hover — this guard flags it
	// only when applied to an input-shaped surface, never globally.
	const files = stylesheetsExcept([APP_CSS]);

	it('no stylesheet sets a background on a bare `input`/`textarea` element selector', () => {
		const violations: string[] = [];
		for (const abs of files) {
			for (const r of rules(styleBlock(readAbs(abs)))) {
				if (bareInputElement(r.selector) && setsBackground(r.body)) {
					violations.push(`${rel(abs)}: bare "${r.selector}" sets a background`);
				}
			}
		}
		expect(violations, 'use a scoped class (e.g. .hb-input), not a bare input/textarea selector').toEqual([]);
	});

	it('no stylesheet uses --bg-elev2 as an input/textarea background', () => {
		const violations: string[] = [];
		for (const abs of files) {
			for (const r of rules(styleBlock(readAbs(abs)))) {
				// Only flag when the rule targets an input-shaped surface AND fills it with elev2.
				// Cards/surfaces/hover using elev2 are the design-system norm and stay untouched.
				if (targetsInput(r.selector) && bgIsElev2(r.body)) {
					violations.push(`${rel(abs)}: "${r.selector}" uses --bg-elev2 as an input background`);
				}
			}
		}
		expect(violations, 'inputs must use --bg-input, not --bg-elev2').toEqual([]);
	});
});
