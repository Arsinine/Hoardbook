// The "🟢 N online" chip's pure view logic (M9, Track C). Kept out of the Svelte component so the
// gating + the m4 unknown-fallback are unit-tested without a DOM.
//
// Rules:
//   - `show_online_count` off → the chip is hidden entirely.
//   - `online === null` (unknown: no cache yet AND no reachable relay) → render a muted "–", never a
//     misleading "0 online" or a blocking spinner (m4).
//   - `online === 0` (a real, fresh count of zero) → render "0" honestly.

import type { OnlineCount } from './api';

export interface ChipView {
	/** Whether to render the chip at all. */
	show: boolean;
	/** The text to render (empty when `show` is false). */
	label: string;
	/** True when the count is unknown — the UI may style the "–" muted. */
	unknown: boolean;
}

export function onlineChipView(count: OnlineCount | null, showSetting: boolean): ChipView {
	if (!showSetting) {
		return { show: false, label: '', unknown: false };
	}
	if (!count || count.online === null || count.online === undefined) {
		// m4: unknown — a dash, not a fake zero.
		return { show: true, label: '🟢 – on network', unknown: true };
	}
	// M15 W7: "on network" (not "online") so it reads distinctly from the contacts-online count.
	return { show: true, label: `🟢 ${count.online} on network`, unknown: false };
}
