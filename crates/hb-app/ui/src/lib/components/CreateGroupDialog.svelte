<script lang="ts">
	// New-group dialog (M13 W5 Slice 2). Renders regardless of how many groups already exist — today
	// every group picker on Contacts is gated behind `groups.length > 0`, which makes creating a
	// trusted group (the on-ramp to M10 Private collections) an unreachable dead path for a first-time
	// user. This is the "+ New group" entry point that always works.
	interface Props {
		open?: boolean;
		oncreate?: (detail: { name: string; color: string; trusted: boolean }) => void;
		oncancel?: () => void;
	}

	import Modal from './Modal.svelte';

	let { open = false, oncreate, oncancel }: Props = $props();

	// A small fixed palette (8 hues) — simple swatch picker, no color-wheel input.
	const PALETTE = [
		'#e05c5c', '#e08a3c', '#d6c23c', '#6cbf5e',
		'#3fb0a8', '#4d8fe0', '#7d6ce0', '#c15ec2',
	];

	let name = $state('');
	let color = $state(PALETTE[0]);
	let trusted = $state(false);

	let canCreate = $derived(name.trim().length > 0);

	function reset() {
		name = '';
		color = PALETTE[0];
		trusted = false;
	}

	function create() {
		if (!canCreate) return;
		oncreate?.({ name: name.trim(), color, trusted });
		reset();
	}

	function cancel() {
		reset();
		oncancel?.();
	}
</script>

<Modal open={open} title="New group" level="stacked" onclose={cancel}>
	<div class="field">
		<label for="cgd-name">Name</label>
		<input id="cgd-name" type="text" placeholder="e.g. Inner Circle" bind:value={name} onkeydown={(e) => e.key === 'Enter' && create()} />
	</div>
	<div class="field field-spaced">
		<span class="field-label">Color</span>
		<div class="swatch-row">
			{#each PALETTE as hex (hex)}
				<button
					type="button"
					class="swatch"
					class:swatch-selected={color === hex}
					style="background:{hex}"
					aria-label={`Color ${hex}`}
					aria-pressed={color === hex}
					onclick={() => (color = hex)}
				></button>
			{/each}
		</div>
	</div>
	<label class="check field-spaced">
		<input type="checkbox" bind:checked={trusted} />
		Trusted — receives your Private collections
	</label>
	{#snippet actions()}
		<button type="button" class="btn-ghost" onclick={cancel}>Cancel</button>
		<button type="button" class="btn-primary" disabled={!canCreate} onclick={create}>Create</button>
	{/snippet}
</Modal>

<style>
	.field { display: flex; flex-direction: column; gap: 5px; }
	.field-spaced { margin-top: 12px; }
	.field label, .field-label { font-size: 11px; color: var(--fg-muted); font-weight: 500; }
	.field input[type='text'] {
		padding: 6px 9px; background: var(--bg-elev2); color: var(--fg);
		border: 1px solid var(--border); border-radius: 6px; font: inherit;
	}
	.swatch-row { display: flex; flex-wrap: wrap; gap: 8px; }
	.swatch {
		width: 24px; height: 24px; border-radius: 50%;
		border: 2px solid transparent;
		cursor: pointer;
		padding: 0;
	}
	.swatch-selected { border-color: var(--fg); box-shadow: 0 0 0 2px var(--bg-elev1); }
	.check { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--fg-muted); }
	/* M15 W1/W2: buttons + modal shell unified (app.css .btn system + Modal.svelte). */
</style>
