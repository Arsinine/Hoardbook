// QURATOR-209 — pure placement seam for OverflowMenu's flip-and-clamp arithmetic.
//
// The old placement was `top: r.bottom + 4, left: Math.max(8, r.right - 200)`: it clamped only the
// LEFT window edge, never flipped vertically (a low trigger pushed the menu below the window floor
// — the case the owner hit), and hardcoded a 200px width guess that misplaces the 240px/260px
// menus even away from any edge. This module holds only the arithmetic so it can be unit-tested
// with synthetic rects; the DOM measurement (anchor rect, menu rect, viewport) happens in the
// component's placement effect after the menu has rendered.

/** The fields of DOMRect the placement needs from the trigger element. */
export interface AnchorRect {
	top: number;
	right: number;
	bottom: number;
}

/** The menu's measured size (width is driven by the `minWidth` prop + content, never assumed). */
export interface MenuRect {
	width: number;
	height: number;
}

export interface Viewport {
	width: number;
	height: number;
}

/** Margin kept between the menu and each viewport edge. */
export const MENU_GAP = 8;

/**
 * Place a menu for the given anchor: right-aligned to the trigger, clamped inside the viewport on
 * BOTH horizontal edges, and flipped above the trigger when it would otherwise cross the window
 * floor. With room to spare it lands exactly at `anchor.bottom + 4 / anchor.right - menu.width`.
 */
export function menuPosition(
	anchor: AnchorRect,
	menu: MenuRect,
	viewport: Viewport,
	gap = MENU_GAP,
): { top: number; left: number } {
	// Horizontal: prefer right-aligned to the anchor. The right clamp is itself floored at `gap`
	// so a menu wider than the viewport hugs the left edge instead of going negative.
	const left = Math.min(
		Math.max(gap, anchor.right - menu.width),
		Math.max(gap, viewport.width - menu.width - gap),
	);
	// Vertical: below the trigger unless the menu would overflow the window floor, then flip above
	// (still clamped, so a menu taller than the window tops out at the gap).
	const below = anchor.bottom + 4;
	const top =
		below + menu.height > viewport.height - gap
			? Math.max(gap, anchor.top - 4 - menu.height)
			: below;
	return { top, left };
}
