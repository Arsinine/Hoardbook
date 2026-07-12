//! M15 W2 — pure focus-trap helpers for Modal.svelte. Kept out of the component so the wrap logic is
//! node-testable (jsdom focus pixels are flaky; assert the selection math instead).

/** Focusable elements inside `root`, in DOM order: interactive controls + anything with a
 *  non-negative tabindex, excluding disabled controls and `tabindex="-1"`. Hidden elements are left
 *  in (jsdom has no layout; the modal is open so its children are visible in practice). */
export function focusableWithin(root: HTMLElement): HTMLElement[] {
	const sel = [
		'button',
		'input',
		'select',
		'textarea',
		'a[href]',
		'[tabindex]:not([tabindex="-1"])',
	].join(',');
	return Array.from(root.querySelectorAll<HTMLElement>(sel)).filter(
		(el) => !el.hasAttribute('disabled') && el.getAttribute('tabindex') !== '-1',
	);
}

/** The element that should receive focus when Tab (or Shift+Tab) is pressed, wrapping at both ends.
 *  Returns null for an empty list. If `active` isn't in the list, Tab lands on the first element and
 *  Shift+Tab on the last (treats the trap boundary as just-outside). */
export function nextFocus(
	list: HTMLElement[],
	active: Element | null,
	shift: boolean,
): HTMLElement | null {
	if (list.length === 0) return null;
	const i = active ? list.indexOf(active as HTMLElement) : -1;
	if (i === -1) return shift ? list[list.length - 1] : list[0];
	const n = shift ? i - 1 : i + 1;
	// Wrap: past the end → first; before the start → last.
	return list[(n + list.length) % list.length];
}
