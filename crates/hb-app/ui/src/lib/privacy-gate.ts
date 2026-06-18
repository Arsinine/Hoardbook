// The one-time pre-first-download IP-exposure notice (spec §Onboarding / §Privacy Model). Browsing
// is relay-private; a download is the one direct, IP-exposing moment — so the notice is a *gate*
// shown before the first download, acknowledged once, and recorded in settings.json.
//
// The pure gate logic (block-until-acknowledged) is unit-tested in `onboarding.ts::attemptDownload`;
// this is the runtime wiring over the real settings + dialog.

import { confirm } from '@tauri-apps/plugin-dialog';
import { acknowledgePrivacyNotice, getSettings } from './api.js';

const NOTICE =
	'Browsing goes through relays, so the hoarders you browse do NOT see your IP.\n\n' +
	'The one exception is downloading an actual file: that connects you directly to that peer, ' +
	'who will see your IP (and you theirs), like BitTorrent.\n\n' +
	"Browse freely. If a direct download isn't acceptable on this network, just don't download here.";

/** Returns true if a download may proceed: already acknowledged, or acknowledged just now. The
 *  notice is shown at most once (recorded in settings.json). */
export async function ensureDownloadPrivacyAck(): Promise<boolean> {
	const s = await getSettings().catch(() => null);
	if (s?.privacy_notice_acknowledged) return true;
	const ok = await confirm(NOTICE, {
		title: 'How Hoardbook connects',
		kind: 'info',
		okLabel: 'I understand',
	});
	if (!ok) return false;
	await acknowledgePrivacyNotice().catch(() => {});
	return true;
}
