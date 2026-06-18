// Updater UX view-model (spec §Auto-updater threat model): the visible-after "now running vX.Y"
// notice and the confirm-before-apply toggle. The once-per-version semantics live in the backend
// (`take_update_notice`); this renders what the backend hands back.

import type { Settings, UpdateApplyMode, UpdateNotice } from './api.js';

export interface UpdateNoticeVM {
	show: boolean;
	version?: string;
}

export function updateNoticeVM(notice: UpdateNotice | null): UpdateNoticeVM {
	return notice ? { show: true, version: notice.version } : { show: false };
}

/** Toggle between Obsidian auto-apply and confirm-before-apply. */
export function nextApplyMode(current: UpdateApplyMode): UpdateApplyMode {
	return current === 'auto' ? 'confirm' : 'auto';
}

export function withApplyMode(settings: Settings, mode: UpdateApplyMode): Settings {
	return { ...settings, update_apply_mode: mode };
}
