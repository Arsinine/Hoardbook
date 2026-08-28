// @vitest-environment jsdom
// QURATOR-139 — "Clicking the hoardbook logo top left should open up a browser page to the github
// repository."
//
// MOUNT test, not a source scan (§9 / P-4: the route pages CAN be mounted; source scans are the
// documented origin of vacuous controls here). The two things only a render can pin:
//   1. the brand is KEYBOARD-REACHABLE — a real <button> with the handler wired, not an onclick on
//      a bare div (unreachable by keyboard, which is an accessibility bug, not a nitpick).
//   2. activating it calls the NEW Tauri command (`open_repo_page`), not an <a href> that would
//      navigate the webview away from the app. The api module is mocked, so nothing shells out.
//
// The URL itself lives in Rust (`commands::diagnostics::REPO_URL`) and is deliberately NOT
// assertable from here — that is the point of hard-coding it server-side: the webview cannot aim
// the opener anywhere else, and equally cannot be tested into claiming it does.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, waitFor, fireEvent } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import Layout from './+layout.svelte';
import { openRepoPage } from '$lib/api.js';
import { toastMessage } from '$lib/stores.js';

// The layout takes a `children` snippet (the routed page). A one-node placeholder keeps the render
// to the shell — the brand lives in the sidebar, not in any page. (It must render at least one
// real node: an empty render string leaves Svelte's snippet anchor with no valid sibling and blows
// up in `get_next_sibling`.)
const emptyChildren = createRawSnippet(() => ({ render: () => `<div data-testid="routed"></div>` }));

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/') }) };
});
vi.mock('$app/stores', () => stubPage);
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
// The layout's own Tauri-module touchpoints, stubbed so nothing dereferences
// window.__TAURI_INTERNALS__ (absent in jsdom) — the same set error-toast-sticky.test.ts stubs,
// which is the repo's established pattern for mounting the layout shell.
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.18.0') }));
// The layout mounts WindowControls, whose onMount calls getCurrentWindow() — that dereferences
// window.__TAURI_INTERNALS__.metadata, which jsdom has not got.
vi.mock('@tauri-apps/api/window', () => ({
	getCurrentWindow: () => ({
		isMaximized: async () => false,
		onResized: async () => () => {},
		minimize: async () => {},
		toggleMaximize: async () => {},
		close: async () => {},
	}),
}));

// The layout's onMount pulls a wide api surface (identity, inbox, announcements). Every mock
// resolves empty so no branch of the shell is left waiting on a rejected promise.
vi.mock('$lib/api.js', () => ({
	getIdentity: vi.fn().mockResolvedValue(null),
	getProfile: vi.fn().mockResolvedValue(null),
	getCollections: vi.fn().mockResolvedValue([]),
	getContacts: vi.fn().mockResolvedValue([]),
	getMessages: vi.fn().mockResolvedValue([]),
	getReadState: vi.fn().mockResolvedValue({}),
	topicAnnouncements: vi.fn().mockResolvedValue([]),
	topicAnnounceSeen: vi.fn().mockResolvedValue({}),
	openRepoPage: vi.fn().mockResolvedValue(undefined),
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	toastMessage.set(null);
});

/** The whole .brand block (mark + wordmark) is the click target — not just the 15x20 logo. */
async function theBrand(): Promise<HTMLButtonElement> {
	return await waitFor(() => {
		const el = document.body.querySelector<HTMLElement>('.brand');
		expect(el, 'no .brand rendered').not.toBeNull();
		return el as HTMLButtonElement;
	});
}

describe('QURATOR-139 — brand click opens the GitHub repo in the system browser', () => {
	it('the brand is a real button (keyboard-reachable), not an onclick on a bare div', async () => {
		render(Layout, { children: emptyChildren });
		const brand = await theBrand();
		expect(brand.tagName, 'the click target must be a button, not a div').toBe('BUTTON');
		expect(brand.type).toBe('button');
		// Focusable = tab-reachable. A bare div has no tab stop; that is the whole defect class.
		brand.focus();
		expect(document.activeElement === brand, 'brand is not focusable (no tab stop)').toBe(true);
		// The leave-the-app signal: a title/tooltip, so it does not read as a navigation bug.
		expect(brand.hasAttribute('title')).toBe(true);
	});

	it('clicking the brand invokes the open_repo_page Tauri command', async () => {
		render(Layout, { children: emptyChildren });
		const brand = await theBrand();
		await fireEvent.click(brand);
		await waitFor(() => expect(openRepoPage).toHaveBeenCalledTimes(1));
		// No arguments: the URL is hard-coded in Rust, never passed from the webview.
		expect(vi.mocked(openRepoPage).mock.calls[0]).toEqual([]);
	});

	it('Enter triggers it too — keyboard activation, not mouse-only', async () => {
		render(Layout, { children: emptyChildren });
		const brand = await theBrand();
		// ⚠ jsdom does not implement a <button>'s activation behavior — Enter/Space synthesizing a
		// click is BROWSER-native, not our code, so firing keyDown here proves nothing and this test
		// cannot go the last mile. What it CAN pin (and what makes keyboard activation true) is the
		// precondition: a focusable, native <button> in the tab order with no negative tabindex and
		// no keydown handler of our own to get wrong. A bare div with onclick fails every clause.
		brand.focus();
		expect(document.activeElement === brand, 'brand cannot take focus (no tab stop)').toBe(true);
		expect(brand.tabIndex, 'an explicit negative tabindex would remove it from the tab order').not.toBe(-1);
		expect(
			brand.hasAttribute('onkeydown'),
			'a bespoke keydown handler means activation was re-implemented by hand',
		).toBe(false);
		// And the handler the browser's Enter/Space synthesis would fire is wired to the command.
		await fireEvent.click(brand);
		await waitFor(() => expect(openRepoPage).toHaveBeenCalledTimes(1));
	});

	it('a failed open surfaces as a toast, not silence', async () => {
		vi.mocked(openRepoPage).mockRejectedValueOnce(new Error('spawn failed'));
		render(Layout, { children: emptyChildren });
		const brand = await theBrand();
		await fireEvent.click(brand);
		await waitFor(() =>
			expect(
				document.body.textContent,
				'opener failure must reach the user as a toast',
			).toContain('Could not open the repository page'),
		);
	});

	it('the brand styles itself as clickable (pointer cursor) and signals it leaves the app', async () => {
		render(Layout, { children: emptyChildren });
		const brand = await theBrand();
		// ⚠ jsdom computes no layout, and vitest extracts Svelte's scoped styles out-of-band (no
		// <style> reaches the jsdom document), so NOTHING here can prove the cursor renders — same
		// limit q135-unknown-pill.test.ts documents: styling is only checkable in a real browser
		// via the Playwright + getComputedStyle recipe. Per the P-4 fallback, this reads the SOURCE,
		// with that limit named. The regex slices to the .brand RULE (not the file) so a match
		// elsewhere (a comment, the :hover rule) cannot satisfy it — the W4 lesson.
		// (Anchored at process.cwd() (= crates/hb-app/ui, where vitest runs) rather than
		// `new URL(..., import.meta.url)`: importing a .svelte component makes import.meta.url
		// non-file: under vitest's jsdom transform, and readFileSync rejects it with "The URL must
		// be of scheme file".)
		const src = readFileSync(resolve('src/routes/+layout.svelte'), 'utf8');
		const rule = src.match(/\.brand\s*\{[^}]*\}/);
		expect(rule, 'no .brand rule in the layout stylesheet').not.toBeNull();
		expect(rule![0], '.brand must declare cursor: pointer').toContain('cursor: pointer');

		const tip = brand.getAttribute('title') ?? brand.getAttribute('aria-label') ?? '';
		expect(
			tip.toLowerCase().includes('github'),
			'the tooltip must name GitHub so a logo click does not read as a navigation bug',
		).toBe(true);
	});
});
