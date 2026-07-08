<script lang="ts">
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
	const tipId = `hint-${Math.random().toString(36).slice(2, 9)}`;

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
		expanded = !expanded;
	}
</script>

<span class="hint-wrap">
	<button
		type="button"
		class="hint-trigger"
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
	<span class="hint-tip" id={tipId} role="tooltip" hidden={!expanded}>{text}</span>
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
		position: absolute;
		bottom: calc(100% + 6px);
		left: 0;
		z-index: 200;
		width: max-content;
		max-width: 240px;
		padding: 8px 10px;
		font-size: 11.5px;
		line-height: 1.4;
		color: var(--fg-muted);
		background: var(--bg-elev3);
		border: 1px solid var(--border-strong);
		border-radius: 7px;
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.4);
	}

	.hint-tip[hidden] {
		display: none;
	}
</style>
