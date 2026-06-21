import { describe, expect, it } from 'vitest';
import { updateNoticeVM } from './update-ux.js';
import type { UpdateNotice } from './api.js';

describe('updater UX — visible-after notice', () => {
	it('now_on_version_notice_renders_once_after_update', () => {
		// Simulate the backend take_update_notice once-semantics: returns the notice once, then null.
		let taken = false;
		const backend = (): UpdateNotice | null => (taken ? null : ((taken = true), { version: '1.0.0' }));
		expect(updateNoticeVM(backend())).toEqual({ show: true, version: '1.0.0' });
		expect(updateNoticeVM(backend())).toEqual({ show: false }); // does not re-render next launch
	});
});
