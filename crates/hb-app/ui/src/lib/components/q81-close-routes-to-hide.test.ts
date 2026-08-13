// QURATOR-81 — TRAP 1 regression: the custom close button must route through
// `getCurrentWindow().close()`, which emits `CloseRequested`. The Rust `on_window_event` handler
// (lib.rs ~line 433) intercepts that to `prevent_close()` + `window.hide()` — the app's deliberate
// "close hides to tray, tray Quit exits" design. Wiring close to `.destroy()` or `.exit()` would
// silently convert hide-to-tray into a quit, and NO vitest suite can catch the behavioural
// inversion (jsdom has no window chrome or tray). This source-scan is the only gate that can.
//
// The assertion is on the close HANDLER's body, not just the file — per CLAUDE.md §9, a symbol scan
// over a whole file cannot prove a specific call site. The test slices out the `close_to_tray`
// function and asserts against THAT, so a future edit rewiring it to `.destroy()` reds here.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const src = () =>
	readFileSync(new URL('./WindowControls.svelte', import.meta.url), 'utf8');

describe('QURATOR-81 TRAP 1 — close routes to hide-to-tray, never quit', () => {
	it('the close handler calls .close() (emits CloseRequested → hide), not .destroy() or exit()', () => {
		const s = src();
		// Slice out the close handler's body — not the whole file — so the assertions are scoped to
		// the one function that matters (CLAUDE.md §9: the call site satisfies a whole-file scan).
		const open = s.indexOf('close_to_tray');
		expect(open).toBeGreaterThan(-1);
		const handler = s.slice(open, s.indexOf('}', open) + 1);

		// .close() emits CloseRequested, which the Rust on_window_event handler intercepts to
		// hide-to-tray. This is the correct path.
		expect(handler).toContain('.close()');

		// .destroy() forces close WITHOUT emitting CloseRequested — it bypasses the hide-to-tray
		// handler entirely, converting the deliberate tray design into a silent quit.
		expect(handler).not.toContain('.destroy()');
		// exit() (or app.exit) would kill the process immediately — same regression.
		expect(handler).not.toMatch(/\.exit\s*\(/);
		expect(handler).not.toMatch(/\bdestroy\b/);
	});

	it('the close handler comment names the CloseRequested path so a future reader does not rewire it', () => {
		const s = src();
		// The comment above the handler is the load-bearing documentation for WHY .close() is used
		// instead of .destroy(). Slice a window that includes the preceding comment line, not just
		// the function body. If the comment is gone, the next reader has no signal that .destroy()
		// is the trap.
		const fnOpen = s.indexOf('async function close_to_tray');
		expect(fnOpen).toBeGreaterThan(-1);
		// Grab the 200 chars before the function (the comment block above it).
		const window = s.slice(Math.max(0, fnOpen - 200), fnOpen);
		expect(window).toMatch(/CloseRequested|hide.*tray|prevent_close/i);
	});

	it('minimize and toggle_maximize handlers are present (the other two controls)', () => {
		const s = src();
		expect(s).toMatch(/async\s+function\s+minimize\b/);
		expect(s).toMatch(/async\s+function\s+toggle_maximize\b/);
		expect(s).toContain('.minimize()');
		expect(s).toContain('.toggleMaximize()');
	});

	it('the maximize icon tracks state (restore icon when maximized)', () => {
		// TRAP 4: the maximize button must become a restore icon when the window is maximized.
		const s = src();
		expect(s).toContain('isMaximized()');
		expect(s).toMatch(/\{#if\s+maximized\}/);
		expect(s).toMatch(/onResized/);
	});
});
