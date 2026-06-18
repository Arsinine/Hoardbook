import { describe, expect, it, vi } from 'vitest';
import { updateNoticeVM, nextApplyMode, withApplyMode } from './update-ux.js';
import type { Settings, UpdateNotice } from './api.js';

describe('updater UX — visible-after notice', () => {
	it('now_on_version_notice_renders_once_after_update', () => {
		// Simulate the backend take_update_notice once-semantics: returns the notice once, then null.
		let taken = false;
		const backend = (): UpdateNotice | null => (taken ? null : ((taken = true), { version: '1.0.0' }));
		expect(updateNoticeVM(backend())).toEqual({ show: true, version: '1.0.0' });
		expect(updateNoticeVM(backend())).toEqual({ show: false }); // does not re-render next launch
	});
});

describe('updater UX — confirm-before-apply toggle', () => {
	it('confirm_before_apply_toggle_persists', async () => {
		const base: Settings = {
			relay_urls: [],
			allow_dms: true,
			privacy_notice_acknowledged: false,
			update_apply_mode: 'auto',
			last_seen_version: '',
		};
		expect(nextApplyMode('auto')).toBe('confirm');
		expect(nextApplyMode('confirm')).toBe('auto');

		let saved: Settings | null = null;
		const save = vi.fn(async (s: Settings) => {
			saved = s;
		});
		const toggled = withApplyMode(base, nextApplyMode(base.update_apply_mode));
		await save(toggled);
		expect(save).toHaveBeenCalledOnce();
		expect(saved!.update_apply_mode).toBe('confirm'); // persisted through saveSettings
	});
});
