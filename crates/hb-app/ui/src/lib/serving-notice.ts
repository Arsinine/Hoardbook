// QURATOR-164, build item 4 — the one-time BASELINE disclosure (owner ruling 2026-09-04:
// "just let them know when they start the app its happening"). Everyone serves the collections
// they themselves browsed, in the background; that tier has NO switch and the disclosure asks no
// permission. It is a step in the onboarding wizard (routes/+page.svelte, step 3), not a startup
// dialog: an unprompted modal at launch read as an error. Passing the step records
// `serving_notice_acknowledged` in settings.json.

import { getSettings, saveSettings } from './api.js';

/** Record that the baseline disclosure was shown. Re-saves the whole settings object via the
 *  generic save_settings path — no dedicated command, so this can't drift from the struct. */
export async function acknowledgeServingNotice(): Promise<void> {
	const s = await getSettings();
	await saveSettings({ ...s, serving_notice_acknowledged: true });
}
