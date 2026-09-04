// QURATOR-164, build item 4 — the one-time BASELINE startup disclosure (owner ruling 2026-09-04:
// "just let them know when they start the app its happening"). Everyone serves the collections
// they themselves browsed, in the background; that tier has NO switch and this notice is not
// asking permission — there is no decline, and nothing is gated by it. It informs once, then
// records `serving_notice_acknowledged` in settings.json.
//
// Shape follows the `privacy_notice_acknowledged` precedent (onboarding.ts::shouldShowPrivacyNotice
// + privacy-gate.ts), with the differences the ruling requires: `message` (an OK-only dialog)
// instead of `confirm`, and no return value to gate anything on. The pure predicate lives here so
// it is unit-testable without mounting anything.

import { message } from '@tauri-apps/plugin-dialog';
import { getSettings, saveSettings } from './api.js';

/** The baseline serving notice is shown iff it has not been shown before. */
export function shouldShowServingNotice(settings: { serving_notice_acknowledged: boolean }): boolean {
	return !settings.serving_notice_acknowledged;
}

const NOTICE =
	'Hoardbook works because people pass things on.\n\n' +
	'When you browse a collection someone else published, your copy is also there for the next person ' +
	"who asks for it — your app sends it along in the background, the same way other people's " +
	'apps send collections to you.\n\n' +
	"This only ever involves collections you have browsed yourself, and it isn't optional — " +
	"that's how a peer-to-peer swarm stays alive. The 'Fetch new collections automatically' " +
	'switch in Settings is a separate, optional way to do more: hold more, and be asked for more.';

/** Show the baseline notice at most once. Fire-and-forget at app start: every failure is swallowed
 *  (an unreadable settings file must never block startup, and the notice simply shows again next
 *  launch). Acknowledging re-saves the whole settings object via the generic save_settings path —
 *  no dedicated command, so this can't drift from the struct. */
export async function showServingNoticeOnce(): Promise<void> {
	const s = await getSettings();
	if (!shouldShowServingNotice(s)) return;
	await message(NOTICE, { title: 'How Hoardbook passes things on', kind: 'info' });
	await saveSettings({ ...s, serving_notice_acknowledged: true });
}
