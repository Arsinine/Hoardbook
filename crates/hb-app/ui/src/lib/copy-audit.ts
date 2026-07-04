// Copy-hygiene rule engine (M13 W5 Slice 4 — Q4/Q6 owner rulings). Pure, regex-based (no HTML/JS
// parser); no Svelte, no DOM.
//
//   ONE noun "Contact" — follow/following/followed/follower(s) is retired from ALL user-facing copy.
//   "share" is RESERVED for handing the share code person-to-person — never a stand-in for "publish".
//
// Code identifiers (function/variable names, CSS class names) are NOT renamed — only the copy a
// person reads changes. This engine therefore only looks at what a person actually reads: quoted
// string literals and markup text nodes — never `<style>` blocks, comments, import lines, JS
// identifiers, or (to avoid flagging pre-existing, unrenamed CSS classes like `hit-following`) a
// `class="…"` attribute's value.

/** Phrases removed from a segment before forbidden-word matching — the allowed senses of "share". */
const ALLOWLIST: RegExp[] = [/share[\s-]?code/gi, /SMB share/gi, /network share/gi, /shared client/gi];

export interface ForbiddenHit {
	rule: string;
	segment: string;
}

const FORBIDDEN_RULES: { rule: string; pattern: RegExp }[] = [
	{ rule: 'follow', pattern: /(?<![A-Za-z0-9_])follow(?:ing|ed|er|ers)?(?![A-Za-z0-9_])/i },
	{ rule: 'share-wrong-sense', pattern: /(?<![A-Za-z0-9_])shar(?:ed|es|ing)(?![A-Za-z0-9_])/i },
];

/** Strip everything that is not user-facing copy: `<style>` blocks, `//` and `/* … *\/` comments,
 *  whole `import …` lines, and `class="…"` / `class='…'` attribute values (CSS class names — a code
 *  identifier, not copy). */
function stripNonCopy(source: string): string {
	let s = source;
	s = s.replace(/<style[\s\S]*?<\/style>/gi, '');
	s = s.replace(/\/\*[\s\S]*?\*\//g, '');
	s = s.replace(/class\s*=\s*"[^"]*"/g, '');
	s = s.replace(/class\s*=\s*'[^']*'/g, '');
	s = s
		.split('\n')
		.map((line) => {
			const trimmed = line.trim();
			if (trimmed.startsWith('import ')) return '';
			const idx = line.indexOf('//');
			return idx >= 0 ? line.slice(0, idx) : line;
		})
		.join('\n');
	return s;
}

const STRING_LITERAL_RE = /'([^'\\]*(?:\\.[^'\\]*)*)'|"([^"\\]*(?:\\.[^"\\]*)*)"|`([^`\\]*(?:\\.[^`\\]*)*)`/g;
const MARKUP_TEXT_RE = />([^<>]+)</g;

/** Collect every quoted string literal ('…' "…" `…`) and markup text node (`>text<`, `{expr}`
 *  segments stripped), trimmed, non-empty. Markup text is scanned outside any `<script>` block only —
 *  TS generics/comparisons (`Array<string>`, `a > b`) would otherwise produce noise `>…<` spans. */
export function extractUserFacingSegments(source: string): string[] {
	const stripped = stripNonCopy(source);
	const segments: string[] = [];

	let m: RegExpExecArray | null;
	STRING_LITERAL_RE.lastIndex = 0;
	while ((m = STRING_LITERAL_RE.exec(stripped))) {
		const val = (m[1] ?? m[2] ?? m[3] ?? '').trim();
		if (val) segments.push(val);
	}

	const markupOnly = stripped.replace(/<script[\s\S]*?<\/script>/gi, '');
	MARKUP_TEXT_RE.lastIndex = 0;
	while ((m = MARKUP_TEXT_RE.exec(markupOnly))) {
		const text = m[1].replace(/\{[^}]*\}/g, ' ').trim();
		if (text) segments.push(text);
	}

	return segments;
}

function stripAllowlisted(segment: string): string {
	let s = segment;
	for (const re of ALLOWLIST) s = s.replace(re, '');
	return s;
}

/** Scan a source file's user-facing copy for forbidden words (Q4/Q6). */
export function findForbiddenCopy(source: string): ForbiddenHit[] {
	const hits: ForbiddenHit[] = [];
	for (const segment of extractUserFacingSegments(source)) {
		const cleaned = stripAllowlisted(segment);
		for (const { rule, pattern } of FORBIDDEN_RULES) {
			if (pattern.test(cleaned)) hits.push({ rule, segment });
		}
	}
	return hits;
}
