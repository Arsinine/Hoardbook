// The one-time direct-connection notice (spec §Onboarding / §Privacy Model). Browsing and chat are
// relay-private; fetching a full list over a ticket DIALS the sender, so both sides see each
// other's IP. That is the one moment this notice is for: shown before the first such fetch,
// acknowledged once, recorded in settings.json.
//
// History: this was the pre-first-DOWNLOAD notice, and lost its only caller when v0.9.6 removed
// file transfer (0905b9df). The ticket fetch (redeem_manifest_ticket) reintroduced the same IP
// exposure without it. It is now OK-only (`message`, no decline), like serving-notice.ts: the
// fetch answers an ask the user already sent, and the owner ruling is that neither side clicks to
// approve a manifest exchange — so this informs, it never gates.

import { message } from '@tauri-apps/plugin-dialog';
import { acknowledgePrivacyNotice, getSettings } from './api.js';
import { shouldShowPrivacyNotice } from './onboarding.js';

export const DIRECT_FETCH_NOTICE =
	"Browsing and chat go through relays, so other hoarders don't see your IP address.\n\n" +
	"The one exception is a collection too big to send over relays. Hoardbook fetches its full list " +
	'by connecting straight to whoever sends it, so they see your IP, and you see theirs.\n\n' +
	"If that isn't acceptable on this network, don't ask for full lists here.";

/** Show the notice at most once, before the first direct fetch. Resolves once dismissed (or at
 *  once if already acknowledged) so the caller can sequence the dial after it. Never throws: an
 *  unreadable settings file must not block a fetch the user asked for, and the notice simply shows
 *  again next time. */
export async function showDirectFetchNoticeOnce(): Promise<void> {
	try {
		const s = await getSettings();
		if (!shouldShowPrivacyNotice(s)) return;
		await message(DIRECT_FETCH_NOTICE, { title: 'How Hoardbook connects', kind: 'info' });
		await acknowledgePrivacyNotice();
	} catch {
		// Swallowed by design — see doc comment.
	}
}
