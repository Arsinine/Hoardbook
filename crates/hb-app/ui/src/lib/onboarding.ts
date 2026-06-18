// Pure onboarding-flow view-model logic, extracted from +page.svelte so it is unit-testable
// (spec §Onboarding). Ported off the retired `hb_id` fields onto `npub` (M5).

import type { IdentityInfo } from './types.js';

/** Wizard step resolution. Identity existence alone suppresses the wizard ("absent on all
 *  subsequent launches"); now keyed on `npub`, not the retired `hb_id`. */
export function resolveInitialStep(identity: Pick<IdentityInfo, 'npub'> | null): { obStep: number } {
	if (identity) return { obStep: 4 };
	return { obStep: 1 };
}

/** Step-2 save behaviour: an empty/whitespace display name skips to step 3 without saving. */
export async function simulateObSaveName(
	displayName: string,
	saveFn: () => Promise<void>,
): Promise<{ savedCalled: boolean; nextStep: number }> {
	if (!displayName.trim()) return { savedCalled: false, nextStep: 3 };
	await saveFn();
	return { savedCalled: true, nextStep: 3 };
}

/** The post-generate backup prompt is shown immediately after an identity is generated and until
 *  the user has exported a backup ("if you lose this key your identity is gone"). */
export function shouldShowBackupPrompt(state: { generated: boolean; backedUp: boolean }): boolean {
	return state.generated && !state.backedUp;
}

/** The one-time pre-first-download IP-exposure notice is shown iff it has not been acknowledged. */
export function shouldShowPrivacyNotice(settings: { privacy_notice_acknowledged: boolean }): boolean {
	return !settings.privacy_notice_acknowledged;
}

/** The privacy notice is a *gate*, not just a banner: a download must not proceed until the notice
 *  has been acknowledged. Drives the download through a mockable seam so the gate is testable even
 *  though the real transfer can't run here. */
export async function attemptDownload(opts: {
	acknowledged: boolean;
	download: () => Promise<number>;
}): Promise<{ status: 'needs-ack' } | { status: 'downloading'; id: number }> {
	if (!opts.acknowledged) return { status: 'needs-ack' };
	const id = await opts.download();
	return { status: 'downloading', id };
}

/** Importing an existing Nostr key ALWAYS surfaces the de-pseudonymization linking warning before
 *  committing — there is no offline oracle to tell whether a key is public/Qurator, so we never
 *  skip it (no hardcoded list, no relay lookup). */
export async function proceedImport(opts: {
	warningAcknowledged: boolean;
	importKey: () => Promise<IdentityInfo>;
}): Promise<{ status: 'needs-warning' } | { status: 'imported'; info: IdentityInfo }> {
	if (!opts.warningAcknowledged) return { status: 'needs-warning' };
	return { status: 'imported', info: await opts.importKey() };
}
