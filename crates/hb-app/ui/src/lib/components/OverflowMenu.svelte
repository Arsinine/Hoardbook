<script lang="ts">
	import { tick } from 'svelte';
	import { menuPosition } from '../menu-position.js';

	// M15 W3 — the reusable `⋯` overflow-menu shell extracted from CollectionRow. Renders
	// position:fixed (anchored to the trigger via getBoundingClientRect) so no `overflow:hidden`
	// ancestor can clip it, with a full-viewport backdrop and Escape to close. The menu *contents*
	// are the caller's `children` snippet (menu items, submenus, inline confirm buttons stay there).
	interface Props {
		open: boolean;
		anchor?: HTMLElement;   // the trigger element the menu positions under
		// QURATOR-381 — right-click menus: open at a CURSOR POINT instead of under an element. When
		// set it replaces the anchor rect for placement (same flip-and-clamp); `anchor`, when also
		// given, still serves callers that need an element for their own follow-on anchoring.
		at?: { x: number; y: number };
		onclose: () => void;
		minWidth?: string;      // default 190px
		children: import('svelte').Snippet;
	}

	let { open, anchor, at, onclose, minWidth = '190px', children }: Props = $props();

	let pos = $state({ top: 0, left: 0 });
	let menuEl = $state<HTMLDivElement | undefined>(undefined); // set by bind:this while the {#if open} branch renders

	// Recompute placement each time it opens (anchor may have scrolled since last time). The menu
	// is only measurable once the {#if open} branch has rendered, so this waits one tick before
	// reading menuEl's rect — a same-tick read returns all zeros and the edge flip would never
	// fire. If it closed again before the tick landed, menuEl is undefined — guard, don't assume.
	$effect(() => {
		if (!open || (!anchor && !at)) return;
		const p = at;
		const a = anchor;
		void (async () => {
			await tick();
			if (!menuEl) return;
			const m = menuEl.getBoundingClientRect();
			const vp = { width: window.innerWidth, height: window.innerHeight };
			// QURATOR-381 — `at` REPLACES the anchor rect for placement. The point is reshaped into
			// the AnchorRect menuPosition already understands, so the SAME flip-and-clamp arithmetic
			// applies: `right = x + menu width` left-aligns the menu at the cursor (menuPosition
			// right-aligns to anchor.right), and `bottom = y - 4` reads "below" from just above the
			// point, so the floor-flip parks the menu's bottom 4px above the cursor, not under it.
			if (p) {
				pos = menuPosition(
					{ top: p.y, right: p.x + m.width, bottom: p.y - 4 },
					{ width: m.width, height: m.height },
					vp,
				);
			} else if (a) {
				const r = a.getBoundingClientRect();
				pos = menuPosition(
					{ top: r.top, right: r.right, bottom: r.bottom },
					{ width: m.width, height: m.height },
					vp,
				);
			}
		})();
	});

	// Escape closes. Capture phase + stopPropagation so an open menu consumes Escape as the topmost
	// layer BEFORE any Modal's bubble-phase backdrop handler — otherwise a menu opened inside a modal
	// would be orphaned when the modal's stopPropagation swallowed the event (chorus catch). No such
	// menu-in-modal exists today, but this makes "topmost wins" correct-by-construction.
	$effect(() => {
		if (!open) return;
		function onKey(e: KeyboardEvent) {
			if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); onclose(); }
		}
		document.addEventListener('keydown', onKey, true);
		return () => document.removeEventListener('keydown', onKey, true);
	});
</script>

{#if open}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="menu-backdrop" onclick={onclose}></div>
	<div class="overflow-menu hb-raised" role="menu" bind:this={menuEl} style="top:{pos.top}px; left:{pos.left}px; min-width:{minWidth}">
		{@render children()}
	</div>
{/if}

<style>
	.menu-backdrop { position: fixed; inset: 0; z-index: var(--z-menu); }
	.overflow-menu {
		position: fixed;
		z-index: var(--z-menu);
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: var(--r-card, 8px);
		padding: 4px;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.3);
	}
</style>
