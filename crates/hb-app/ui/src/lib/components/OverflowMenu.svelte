<script lang="ts">
	// M15 W3 — the reusable `⋯` overflow-menu shell extracted from CollectionRow. Renders
	// position:fixed (anchored to the trigger via getBoundingClientRect) so no `overflow:hidden`
	// ancestor can clip it, with a full-viewport backdrop and Escape to close. The menu *contents*
	// are the caller's `children` snippet (menu items, submenus, inline confirm buttons stay there).
	interface Props {
		open: boolean;
		anchor?: HTMLElement;   // the trigger element the menu positions under
		onclose: () => void;
		minWidth?: string;      // default 190px
		children: import('svelte').Snippet;
	}

	let { open, anchor, onclose, minWidth = '190px', children }: Props = $props();

	let pos = $state({ top: 0, left: 0 });

	// Recompute placement each time it opens (anchor may have scrolled since last time).
	$effect(() => {
		if (open && anchor) {
			const r = anchor.getBoundingClientRect();
			pos = { top: r.bottom + 4, left: Math.max(8, r.right - 200) };
		}
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
	<div class="overflow-menu" role="menu" style="top:{pos.top}px; left:{pos.left}px; min-width:{minWidth}">
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
		border-radius: 8px;
		padding: 4px;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.3);
	}
</style>
