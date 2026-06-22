<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { Visibility } from '../types.js';
	import { NOT_DRM_NOTE } from '../private-collections-view.js';

	/** Current visibility. Default Public — a collection is Private only by explicit choice (M10). */
	export let visibility: Visibility = 'Public';
	/** Disable while a save is in flight. */
	export let disabled = false;

	const dispatch = createEventDispatcher<{ change: Visibility }>();
	const selectId = `vis-${Math.random().toString(36).slice(2, 9)}`;

	function onChange(e: Event) {
		const v = (e.currentTarget as HTMLSelectElement).value as Visibility;
		visibility = v;
		dispatch('change', v);
	}
</script>

<div class="vis-selector">
	<label for={selectId}>Visibility</label>
	<select id={selectId} {disabled} value={visibility} on:change={onChange}>
		<option value="Public">Public — anyone with your share code</option>
		<option value="Private">Private — only contacts in a trusted group</option>
	</select>
	{#if visibility === 'Private'}
		<p class="not-drm" role="note">{NOT_DRM_NOTE}</p>
	{/if}
</div>

<style>
	.vis-selector {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.vis-selector label {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg-muted);
	}

	.vis-selector select {
		padding: 5px 8px;
		background: var(--bg-elev2);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 6px;
		font: inherit;
	}

	.not-drm {
		margin: 2px 0 0;
		font-size: 11.5px;
		line-height: 1.4;
		color: var(--fg-dim);
	}
</style>
