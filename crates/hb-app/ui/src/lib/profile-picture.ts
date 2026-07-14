//! Profile-picture apply/remove flow (devtest #1) — used by the Home profile header (the one
//! discoverable spot; Settings no longer changes the picture, devtest #5). Compress a picked image →
//! save it onto the local Profile → republish the teaser if one is already public. The Home caller
//! must sync its editor `form.picture` from the store afterwards (devtest #4) or the next profile
//! publish rewrites `form` and reverts the image.

import { get } from 'svelte/store';
import { saveProfile, hasPublishedProfile, publishProfile } from './api.js';
import { profile, toast } from './stores.js';
import { compressToDataUri } from './image-compress.js';

/** Compress `file` and save+publish it as the profile picture. No-op with an error toast if no
 *  profile exists yet. Returns true on success. */
export async function applyProfilePicture(file: File | Blob): Promise<boolean> {
	const current = get(profile);
	if (!current) {
		toast('Save your profile first, then add a picture', 'error');
		return false;
	}
	try {
		const dataUri = await compressToDataUri(file);
		const updated = { ...current, picture: dataUri };
		await saveProfile(updated);
		profile.set(updated);
		// Republish so a public teaser reflects the new picture immediately (replaceable event).
		if (await hasPublishedProfile()) await publishProfile();
		toast('Picture updated', 'success');
		return true;
	} catch (e) {
		toast(String(e), 'error');
		return false;
	}
}

/** Clear the profile picture and republish if public. */
export async function removeProfilePicture(): Promise<void> {
	const current = get(profile);
	if (!current) return;
	try {
		const updated = { ...current, picture: undefined };
		await saveProfile(updated);
		profile.set(updated);
		if (await hasPublishedProfile()) await publishProfile();
		toast('Picture removed', 'success');
	} catch (e) {
		toast(String(e), 'error');
	}
}
