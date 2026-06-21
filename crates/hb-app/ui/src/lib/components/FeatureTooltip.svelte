<script lang="ts">
	import { TOOLTIPS, type TooltipKey } from '../tooltips.js';

	/** Which of the five spec'd feature-help anchors to render (HOARDBOOK_SPEC §8). */
	export let key: TooltipKey;

	let expanded = false;
	// Unique-per-instance id so aria-describedby wires the trigger → its own copy, even with several
	// FeatureTooltips on one screen.
	const tipId = `ft-${key}-${Math.random().toString(36).slice(2, 9)}`;

	$: content = TOOLTIPS[key];

	function show() {
		expanded = true;
	}
	function hide() {
		expanded = false;
	}
	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') hide();
	}
	function toggle() {
		// Click/tap toggles — still show/hide ONLY; the trigger never performs an action (spec).
		expanded = !expanded;
	}
</script>

<span class="ft-wrap">
	<button
		type="button"
		class="ft-trigger"
		class:has-label={$$slots.default}
		aria-describedby={tipId}
		aria-expanded={expanded}
		aria-label={$$slots.default ? undefined : `More information: ${content.title}`}
		on:mouseenter={show}
		on:mouseleave={hide}
		on:focus={show}
		on:blur={hide}
		on:click={toggle}
		on:keydown={onKeydown}
	>
		<slot><span class="ft-icon" aria-hidden="true">ⓘ</span></slot>
	</button>
	<span class="ft-tip" id={tipId} role="tooltip" hidden={!expanded}>
		<span class="ft-tip-title">{content.title}</span>
		<span class="ft-tip-body">{content.body}</span>
	</span>
</span>

<style>
	.ft-wrap {
		position: relative;
		display: inline-flex;
		align-items: center;
	}

	.ft-trigger {
		display: inline-flex;
		align-items: center;
		background: transparent;
		border: none;
		padding: 0;
		margin: 0 0 0 3px;
		cursor: help;
		color: var(--fg-dim);
		font: inherit;
		line-height: 1;
	}

	/* Dotted-underline variant when the trigger wraps a label instead of the ⓘ marker. */
	.ft-trigger.has-label {
		margin: 0;
		color: inherit;
		text-decoration: underline dotted;
		text-underline-offset: 2px;
	}

	.ft-trigger:hover,
	.ft-trigger:focus-visible {
		color: var(--fg);
	}

	.ft-icon { font-size: 12px; }

	.ft-tip {
		position: absolute;
		bottom: calc(100% + 6px);
		left: 0;
		z-index: 200;
		width: max-content;
		max-width: 280px;
		display: flex;
		flex-direction: column;
		gap: 3px;
		padding: 8px 10px;
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		border-radius: 7px;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.4);
		opacity: 1;
		transition: opacity 0.12s ease;
	}

	.ft-tip[hidden] { display: none; }

	@media (prefers-reduced-motion: reduce) {
		.ft-tip { transition: none; }
	}

	.ft-tip-title {
		font-size: 11.5px;
		font-weight: 600;
		color: var(--fg);
	}

	.ft-tip-body {
		font-size: 11.5px;
		line-height: 1.4;
		color: var(--fg-muted);
	}
</style>
