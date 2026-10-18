// @vitest-environment jsdom
// QURATOR-164 baseline disclosure: it used to be an OK-only dialog fired by the layout at app
// start. Owner, 2026-10-18: a popup at launch reads as an error, so it moved into the onboarding
// wizard (routes/onboarding-sharing-step.test.ts). This file pins the removal: an unacknowledged
// user launching the app gets NO dialog from the layout.
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

describe('QURATOR-164 — no startup dialog', () => {
	it('unacknowledged → the layout shows no dialog and writes no settings', async () => {
		getSettingsMock.mockResolvedValue({ ...SETTINGS });
		render(Layout, { children: emptyChildren });
		await tick();
		// MUTATION (P-10): in routes/+layout.svelte, restore the startup call
		// `message('x', { title: 'x' }).catch(() => { });` inside the onMount IIFE (import
		// `message` from '@tauri-apps/plugin-dialog') — this test reds.
		await new Promise((r) => setTimeout(r, 25));
		expect(messageMock).not.toHaveBeenCalled();
		expect(saveSettingsMock).not.toHaveBeenCalled();
	});
});
