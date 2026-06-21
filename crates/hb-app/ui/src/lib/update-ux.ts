// Updater UX view-model (spec §Auto-updater threat model): the visible-after "now running vX.Y"
// notice. The once-per-version semantics live in the backend (`take_update_notice`); this renders
// what the backend hands back.

import type { UpdateNotice } from './api.js';

export interface UpdateNoticeVM {
	show: boolean;
	version?: string;
}

export function updateNoticeVM(notice: UpdateNotice | null): UpdateNoticeVM {
	return notice ? { show: true, version: notice.version } : { show: false };
}
