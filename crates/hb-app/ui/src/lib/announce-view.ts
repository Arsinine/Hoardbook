// Q1 — topic-announce app wiring (M13 Part A): the button label + gate + explainer copy. Pure so the
// cooldown math and copy are unit-tested without a DOM; the backend (`topic_announce_status`) is the
// authoritative clock — this module only renders what it returns.

/** True once the cooldown has fully elapsed (0 remaining). */
export function canAnnounce(remainingSecs: number): boolean {
	return remainingSecs <= 0;
}

/** The Announce button's label — plain "Announce" when ready, else the minutes remaining (ceiling-
 *  rounded, floored to a minimum of 1 so a sub-minute remainder never reads as "ready in 0 min"). */
export function cooldownLabel(remainingSecs: number): string {
	if (canAnnounce(remainingSecs)) return 'Announce';
	const mins = Math.max(1, Math.ceil(remainingSecs / 60));
	return `Announce — ready in ${mins} min`;
}

export const ANNOUNCE_EXPLAINER =
	"Pushes a highlighted notice to all members' channel view for 24h. Limited to one per hour.";
