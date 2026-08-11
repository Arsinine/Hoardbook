// The "N online" chip's pure view logic (M9, Track C). Kept out of the Svelte component so the
// gating, the m4 unknown-fallback, and the QURATOR-67 relay-health colour are unit-tested without a DOM.
//
// Rules (QURATOR-67 — owner ruled):
//   - `show_online_count` off → the chip is hidden entirely.
//   - Relay health is checked FIRST: if no relay reports `connected`, the state is red regardless
//     of the count. A disconnected client must never show "3 on network" in green while the user's
//     own machine is unplugged — that confusion took five presence reports to diagnose.
//   - `online === null` (unknown) → a muted dash, never a fake zero (m4). State is RED.
//   - `online === 0` (relays connected, a real fresh zero) → amber + "0" honestly.
//   - `online >= 1` (relays connected) → green + the count. Exactly 1 is green, not amber.

import type { OnlineCount, RelayHealth } from './api';

export interface ChipView {
	/** Whether to render the chip at all. */
	show: boolean;
	/** The text to render (empty when `show` is false). No colour emoji — the dot comes from `state`. */
	label: string;
	/** True when the count is unknown — the UI may style the dash muted. */
	unknown: boolean;
	/** QURATOR-67 — drives the coloured dot: red (relays down / unknown), amber (0), green (≥1). */
	state: 'red' | 'amber' | 'green';
}

export function onlineChipView(
	count: OnlineCount | null,
	showSetting: boolean,
	relays: RelayHealth[],
): ChipView {
	if (!showSetting) {
		return { show: false, label: '', unknown: false, state: 'red' };
	}
	// QURATOR-67: relay health FIRST. A stale non-zero count in the cache must not paint the chip
	// green/amber when the client itself cannot reach any relay — the red dot is the signal that
	// "the number you see may be wrong because YOUR machine is disconnected."
	const relaysConnected = relays.some((r) => r.connected);
	if (!relaysConnected) {
		return { show: true, label: '– on network', unknown: true, state: 'red' };
	}
	// Relays connected. Now the count decides — but unknown is STILL red (owner ruling).
	if (!count || count.online === null || count.online === undefined) {
		// m4: unknown — a dash, not a fake zero.
		return { show: true, label: '– on network', unknown: true, state: 'red' };
	}
	if (count.online === 0) {
		// Real zero, relays up — amber, not green.
		return { show: true, label: '0 on network', unknown: false, state: 'amber' };
	}
	// Relays connected, count >= 1 — green (including exactly 1).
	return { show: true, label: `${count.online} on network`, unknown: false, state: 'green' };
}
