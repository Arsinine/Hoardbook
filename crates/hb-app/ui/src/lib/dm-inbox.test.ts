import { describe, expect, it } from 'vitest';
import { petnameFor, type Contact } from './identity-display.js';
import type { ReceivedMessage } from './types.js';

// M4: a DM is NIP-17 gift-wrapped, so the relay-visible wrap key is ephemeral and meaningless.
// `ReceivedMessage.from` is the REAL sender npub recovered from inside the seal (hb-net::unwrap_dm).
// The inbox row resolves that npub through the same impersonation-resistant petname layer the
// browse UI uses (petname binds to the npub, never the display name).

const ALICE = 'npub1alice';
const IMPOSTOR = 'npub1impostor';

function dm(from: string, content: string): ReceivedMessage {
	return { from, to: 'npub1me', content, sent_at: '2026-06-17T00:00:00Z' };
}

describe('DM inbox — sender resolves from the real (post-unwrap) npub', () => {
	const contacts: Contact[] = [{ npub: ALICE, petname: 'AliceHoarder' }];

	it('renders a saved contact DM under their verified petname (bound to npub)', () => {
		const msg = dm(ALICE, 'back room is open');
		const label = petnameFor(msg.from, 'whatever-display-name', contacts);
		expect(label.label).toBe('AliceHoarder');
		expect(label.verified).toBe(true);
		expect(label.stranger).toBe(false);
	});

	it('flags an impostor reusing a contact petname under a different npub', () => {
		// A hostile sender sets their teaser display_name to a contact's petname, but the seal
		// reveals a different npub → impersonation alert, never the petname.
		const msg = dm(IMPOSTOR, 'it me, alice');
		const label = petnameFor(msg.from, 'AliceHoarder', contacts);
		expect(label.verified).toBe(false);
		expect(label.warning).toContain('different key');
	});

	it('marks an unknown sender as an unverified stranger', () => {
		const msg = dm('npub1stranger', 'hello');
		const label = petnameFor(msg.from, 'Stranger', contacts);
		expect(label.stranger).toBe(true);
		expect(label.label).toBe('Stranger');
	});
});
