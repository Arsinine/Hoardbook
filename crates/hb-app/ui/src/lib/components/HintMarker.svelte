<script lang="ts">
	import { tick, onDestroy } from 'svelte';
	// A small Windows-style "?" help marker that reveals a short field hint on hover/focus. Distinct
	// from <FeatureTooltip> (the pinned §8 feature-help registry): this carries FREE field-hint text,
	// so per-field hints don't have to be registered as §8 anchors. Explanatory only — never gates an
	// action. Accessibility mirrors FeatureTooltip: a real <button> trigger, aria-describedby → its own
	// role=tooltip copy, hover/focus to show, Escape/blur to hide.

	
	
	interface Props {
		/** The hint text shown on hover/focus. */
		text: string;
		/** Accessible-name fragment for the icon-only trigger (e.g. the field label). */
		label?: string;
	}

	let { text, label = 'field' }: Props = $props();

	let expanded = $state(false);
	let trigger: HTMLElement | undefined = $state();
	let tipEl: HTMLElement | undefined = $state();
	// Inline placement, viewport px (QURATOR-383): the tip is position:fixed while open so overflow
	// ancestors (.col max-height:60vh/overflow-y:auto in AddCollectionModal, .screen-frame,
	// .modal-card) cannot clip it; empty until measured — the CSS absolute fallback still styles it.
	let tipTop = $state('');
	let tipLeft = $state('');
	const tipId = `hint-${Math.random().toString(36).slice(2, 9)}`;

	const GAP = 6;
	// jsdom computes no layout (a measured tip rect is all zeros), so fall back to a conservative
	// tip size for the flip/clamp maths; real browsers measure the rendered tip.
	const TIP_FALLBACK_H = 120;
	const TIP_FALLBACK_W = 240;

	async function place() {
		if (!trigger) return;
		const rect = trigger.getBoundingClientRect();
		await tick(); // the tip must be rendered before it can be measured
		const tRect = tipEl?.getBoundingClientRect();
		const tipH = tRect && tRect.height > 0 ? tRect.height : TIP_FALLBACK_H;
		const tipW = tRect && tRect.width > 0 ? tRect.width : TIP_FALLBACK_W;
		// Default ABOVE the trigger; flip below when the bubble would not fit above it
		// (tip height + a 12px margin against the viewport edge).
		// MUTATION (P-10): forcing `const below = false;` (flip branch reverted, always place above)
		// redd `near the viewport top, the tip is position:fixed and flips BELOW the trigger`. Seen red.
		const below = rect.top < tipH + 12;
		const maxLeft = Math.max(8, window.innerWidth - tipW - 8);
		const left = Math.min(Math.max(rect.left, 8), maxLeft);
		const top = below ? rect.bottom + GAP : rect.top - tipH - GAP;
		tipLeft = String(left);
		tipTop = String(top);
		// A backdrop-filter ancestor is the containing block for a fixed child, not the viewport:
		// glass themes give the modal card `.hb-raised` → backdrop-filter, so `left: Npx` lands N px
		// from the CARD, not the window (seen in the browser 2026-10-18: off by the card's own origin).
		// Measure where it actually landed and subtract the offset — exact, since the offset is constant.
		// Skipped when nothing is laid out (jsdom: zero rect).
		await tick();
		const placed = tipEl?.getBoundingClientRect();
		if (placed && placed.width > 0) {
			tipLeft = String(left - (placed.left - left));
			tipTop = String(top - (placed.top - top));
		}
	}

	async function show() {
		expanded = true;
		if (typeof window === 'undefined') return;
		// A fixed tip must never float detached from its trigger: close on scroll/resize while open.
		window.addEventListener('scroll', hide, { capture: true });
		window.addEventListener('resize', hide);
		await place();
	}
	function hide() {
		expanded = false;
		tipTop = '';
		tipLeft = '';
		if (typeof window === 'undefined') return;
		window.removeEventListener('scroll', hide, { capture: true });
		window.removeEventListener('resize', hide);
	}
	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') hide();
	}
	function toggle() {
		if (expanded) hide();
		else void show();
	}
	onDestroy(() => {
		if (typeof window === 'undefined') return;
		window.removeEventListener('scroll', hide, { capture: true });
		window.removeEventListener('resize', hide);
	});
</script>

<span class="hint-wrap">
	<button
		type="button"
		class="hint-trigger"
		bind:this={trigger}
		aria-describedby={tipId}
		aria-expanded={expanded}
		aria-label={`More information: ${label}`}
		onmouseenter={show}
		onmouseleave={hide}
		onfocus={show}
		onblur={hide}
		onclick={toggle}
		onkeydown={onKeydown}
	>
		<span class="hint-icon" aria-hidden="true">?</span>
	</button>
	<span
		class="hint-tip hb-raised"
		id={tipId}
		role="tooltip"
		hidden={!expanded}
		bind:this={tipEl}
		style={expanded && tipTop !== ''
			? `position: fixed; top: ${tipTop}px; left: ${tipLeft}px; bottom: auto;`
			: ''}
	>{text}</span>
</span>

<style>
	.hint-wrap {
		position: relative;
		display: inline-flex;
		align-items: center;
	}

	.hint-trigger {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 14px;
		height: 14px;
		padding: 0;
		margin: 0 0 0 4px;
		background: transparent;
		border: 1px solid var(--border-strong);
		border-radius: 50%;
		cursor: help;
		color: var(--fg-dim);
		font: inherit;
		line-height: 1;
	}

	.hint-trigger:hover,
	.hint-trigger:focus-visible {
		color: var(--fg);
		border-color: var(--fg-muted);
	}

	.hint-icon {
		font-size: 10px;
		font-weight: 700;
	}

	.hint-tip {
		/* QURATOR-383: kept as the closed/no-JS fallback only — while open, place() overrides it with
		   an INLINE position:fixed at viewport coordinates, because scroll containers (.col
		   max-height:60vh/overflow-y:auto in AddCollectionModal; .screen-frame; .modal-card) clip
		   absolutely-positioned descendants regardless of z-index.
		   ⚠ A backdrop-filter ancestor is the containing block for fixed descendants. In glass themes
		   the modal card is `.hb-raised` (app.css), which carries backdrop-filter, so place()
		   measures the landed position and corrects by the offset rather than assuming viewport
		   coordinates.
		   z-index stays var(--z-overlay): the tip sits inside the modal's stacking context and paints
		   above the modal card; nothing was observed painting over it. */
		position: absolute;
		bottom: calc(100% + 6px);
		left: 0;
		z-index: var(--z-overlay);
		width: max-content;
		max-width: 240px;
		padding: 8px 10px;
		font-size: 11.5px;
		line-height: 1.4;
		color: var(--fg-muted);
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		/* Multi-line bubble — same precedent as .hb-textarea (app.css) and FeatureTooltip's .ft-tip:
		   five themes set --r-control to 999px (pill controls), which turns a wrapping box into a
		   stadium and clips its text. */
		border-radius: min(var(--r-control, 7px), var(--r-card, 10px));
		box-shadow: var(--shadow-pop);
	}

	.hint-tip[hidden] {
		display: none;
	}
</style>
