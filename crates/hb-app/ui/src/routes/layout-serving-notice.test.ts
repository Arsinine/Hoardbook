// @vitest-environment jsdom
// QURATOR-164 build item 4 — the BASELINE startup disclosure wiring. The pure predicate is unit-
// tested in src/lib/serving-notice.test.ts; this file pins the RUNTIME half: on app start, when
// the notice is unacknowledged, the real mounted layout shows it (via the OK-only `message`
// dialog — a notice, not consent) and persists `serving_notice_acknowledged: true` through the
// generic save_settings path with every other settings field intact. And when it IS already
// acknowledged, no dialog and no save fire — "disclosed once", not every launch.
//
// Mounting follows layout-listen-catch.test.ts / q139-brand-github-link.test.ts (the repo's
// established layout-shell pattern: $app/stores + Tauri modules stubbed so nothing dereferences
// window.__TAURI_INTERNALS__).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { readable } from 'svelte/store';
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

const emptyChildren = createRawSnippet(() => ({ render: () => `<div data-testid="routed"></div>` }));

const SETTINGS = {
	relay_urls: ['wss://relay.example.com'],
	allow_dms: true,
	privacy_notice_acknowledged: true,
	last_seen_version: '',
	snapshot_auto_update: true,
	snapshot_reconcile_poll: false,
	show_online_count: true,
	discoverable: false,
	big_relay_url: '',
	swarm_caching: false,
	serving_notice_acknowledged: false,
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-164 — baseline serving notice fires once at app start', () => {
	it('unacknowledged → the notice dialog shows and the flag persists with the whole settings object', async () => {
		getSettingsMock.mockResolvedValue({ ...SETTINGS });
		render(Layout, { children: emptyChildren });
		await tick();

		await vi.waitFor(() => expect(messageMock).toHaveBeenCalledTimes(1));
		// The notice is the tier description, not a consent ask — assert it names what's happening.
		expect(String(messageMock.mock.calls[0]![0])).toContain('pass things on');
		// MUTATION (P-10): in src/lib/serving-notice.ts, in the `showServingNoticeOnce` function
		// body, change `await saveSettings({ ...s, serving_notice_acknowledged: true });` to
		// `await saveSettings({ ...s });` — this test reds on the flag assert below.
		await vi.waitFor(() => expect(saveSettingsMock).toHaveBeenCalledTimes(1));
		const saved = saveSettingsMock.mock.calls[0]![0];
		expect(saved.serving_notice_acknowledged).toBe(true);
		// The whole object rides along — the acknowledge write must not clobber any other field.
		expect(saved.relay_urls).toEqual(['wss://relay.example.com']);
		expect(saved.swarm_caching).toBe(false);
	});

	it('already acknowledged → no dialog, no save (disclosed once, not every launch)', async () => {
		getSettingsMock.mockResolvedValue({ ...SETTINGS, serving_notice_acknowledged: true });
		render(Layout, { children: emptyChildren });
		await tick();
		// MUTATION (P-10): in src/lib/serving-notice.ts, in the `showServingNoticeOnce` function
		// body, delete the `if (!shouldShowServingNotice(s)) return;` early-out — this test reds:
		// the dialog fires again on a user who already saw it.
		await new Promise((r) => setTimeout(r, 25));
		expect(messageMock).not.toHaveBeenCalled();
		expect(saveSettingsMock).not.toHaveBeenCalled();
	});
});
