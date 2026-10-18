// @vitest-environment jsdom
// v0.24.0 devtest item 8 — "the panel corner roundings disappeared after v0.23.2 … now its all
// sharp corners." Cause: 8bc4f2f4 (Mica removal) deleted the `applyAppearance(...)` call from the
// layout's Settings pass along with its see-through arguments, leaving `const s = await
// getSettings();` with nothing after it. Settings were then NEVER applied at launch; the app ran on
// whatever the boot script painted from the localStorage mirror, and a stale glass:false mirror
// meant the flush, square glass-off layout no matter what Settings said.
//
// Mounted layout (same shell as layout-no-startup-dialog.test.ts). The mirror is seeded with the
// OPPOSITE of Settings, so only the Settings pass can produce the asserted attributes.
// P-10 MUTATION (orchestrator): in +layout.svelte delete the line
// `applyAppearance({ theme: (s.theme ?? DEFAULT_THEME) as ThemeId, glass: s.glass ?? true });` → REDS.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';
import { createRawSnippet } from 'svelte';

const stubPage = vi.hoisted(async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/') }) };
});
vi.mock('$app/stores', () => stubPage);
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.18.0') }));
vi.mock('@tauri-apps/api/window', () => ({
	getCurrentWindow: () => ({
		isMaximized: async () => false,
		onResized: async () => () => {},
		minimize: async () => {},
		toggleMaximize: async () => {},
		close: async () => {},
	}),
}));

// `message` is the notice surface: OK-only, no decline button — the notice-vs-consent line the
// owner ruling draws. Recorded so the test can prove the dialog (not just the save) fired.
const messageMock = vi.hoisted(() => vi.fn(async (_text: string, _opts?: unknown) => ({ ok: true })));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn(), message: messageMock }));

const getSettingsMock = vi.hoisted(() => vi.fn());
const saveSettingsMock = vi.hoisted(() => vi.fn(async (_s: Record<string, unknown>) => undefined));
vi.mock('$lib/api.js', () => ({
	getIdentity: vi.fn(async () => null),
	getProfile: vi.fn(async () => null),
	getCollections: vi.fn(async () => []),
	getContacts: vi.fn(async () => []),
	getMessages: vi.fn(async () => []),
	getReadState: vi.fn(async () => ({})),
	topicAnnouncements: vi.fn(async () => []),
	topicAnnounceSeen: vi.fn(async () => ({})),
	openRepoPage: vi.fn(),
	getSettings: getSettingsMock,
	saveSettings: saveSettingsMock,
}));

import Layout from './+layout.svelte';
import { APPEARANCE_KEY } from '$lib/themes/apply.js';

const emptyChildren = createRawSnippet(() => ({ render: () => `<div data-testid="routed"></div>` }));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	localStorage.clear();
	for (const n of ['data-theme', 'data-glass-float', 'data-glass-off']) document.documentElement.removeAttribute(n);
});

describe('v0.24.0 item 8 — the layout applies the SAVED appearance at launch', () => {
	it('Settings win over a stale boot mirror (theme and glass)', async () => {
		// Stale mirror: glass off, Hoard — what the boot script would have painted.
		localStorage.setItem(APPEARANCE_KEY, JSON.stringify({ theme: 'hoard', glass: false }));
		document.documentElement.setAttribute('data-glass-off', '');
		getSettingsMock.mockResolvedValue({ relay_urls: [], allow_dms: true, serving_notice_acknowledged: true, theme: 'sky-slate', glass: true });
		render(Layout, { children: emptyChildren });
		await tick();
		await vi.waitFor(() => expect(document.documentElement.getAttribute('data-theme')).toBe('sky-slate'));
		expect(document.documentElement.hasAttribute('data-glass-off'), 'saved glass:true clears glass-off').toBe(false);
		expect(document.documentElement.hasAttribute('data-glass-float')).toBe(true);
		// …and the mirror is corrected, so the NEXT launch's first frame is right too.
		expect(JSON.parse(localStorage.getItem(APPEARANCE_KEY)!)).toEqual({ theme: 'sky-slate', glass: true });
	});
});
