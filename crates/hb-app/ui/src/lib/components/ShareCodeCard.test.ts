// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import ShareCodeCard from './ShareCodeCard.svelte';
import type { ShareCodeInfo } from '../api.js';

afterEach(cleanup);

// The 'same' actionable state: the embedded npub IS the chat peer, code carries a browse key,
// and no saved contact holds one yet — the one state that renders "Unlock browsing".
const info: ShareCodeInfo = {
	npub: 'npub1peer',
	fingerprint: { words: ['ember', 'quartz', 'lattice', 'tarn', 'mint'], colorHex: '#5588aa' },
	has_browse_key: true,
};
const base = {
	info,
	chatPeerNpub: 'npub1peer',
	ownNpub: 'npub1me',
	contacts: [],
	quarantined: false,
	unlocked: false,
	onunlock: () => {},
	onaddcontact: () => {},
};

describe('ShareCodeCard — Unlock pending state (the unlock runs pasteKey→follow over relays)', () => {
	it('idle: an enabled "Unlock browsing" button', () => {
		const { getByRole } = render(ShareCodeCard, { props: { ...base, unlocking: false } });
		const btn = getByRole('button', { name: 'Unlock browsing' }) as HTMLButtonElement;
		expect(btn.disabled).toBe(false);
	});

	it('unlocking: the button is disabled and acknowledges the click as "Unlocking…"', () => {
		const { getByRole } = render(ShareCodeCard, { props: { ...base, unlocking: true } });
		const btn = getByRole('button', { name: 'Unlocking…' }) as HTMLButtonElement;
		expect(btn.disabled).toBe(true);
	});
});
