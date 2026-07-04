<script lang="ts">
	// Shared inline two-step confirm (M13 W5) — mirrors the Settings "Wipe all data" pattern (a
	// boolean reveal → "Are you sure? …" + Confirm/Cancel), now reusable for every destructive action
	// (collection Remove, contact Remove, Topic Leave) instead of firing straight from a single click.
	import { createEventDispatcher } from 'svelte';

	/** The resting trigger's label (e.g. "Remove", "Leave"). */
	export let label = 'Remove';
	/** The revealed prompt text. */
	export let confirmText = 'Are you sure?';
	export let confirmLabel = 'Confirm';
	export let cancelLabel = 'Cancel';
	/** Styles the trigger + confirm button as destructive (red). */
	export let danger = true;
	export let disabled = false;

	const dispatch = createEventDispatcher<{ confirm: void }>();

	let revealed = false;

	function start() {
		if (disabled) return;
		revealed = true;
	}
	function cancel() {
		revealed = false;
	}
	function confirm() {
		revealed = false;
		dispatch('confirm');
	}
</script>

{#if !revealed}
	<button type="button" class="cb-trigger" class:cb-danger={danger} on:click={start} {disabled} {...$$restProps}>
		{label}
	</button>
{:else}
	<span class="cb-confirm">
		<span class="cb-confirm-text">{confirmText}</span>
		<button type="button" class="cb-yes" class:cb-danger={danger} on:click={confirm}>{confirmLabel}</button>
		<button type="button" class="cb-no" on:click={cancel}>{cancelLabel}</button>
	</span>
{/if}

<style>
	.cb-trigger {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 5px 11px;
		font-family: var(--font-ui);
		font-size: 12px;
		font-weight: 500;
		color: var(--fg-muted);
		background: transparent;
		border: 1px solid transparent;
		border-radius: 7px;
		cursor: pointer;
		white-space: nowrap;
		line-height: 1;
	}
	.cb-trigger.cb-danger { color: var(--error, #e05c5c); }
	.cb-trigger:disabled { opacity: 0.5; cursor: not-allowed; }

	.cb-confirm {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		flex-wrap: nowrap;
	}

	.cb-confirm-text {
		font-size: 11.5px;
		color: var(--error, #e05c5c);
		white-space: nowrap;
	}

	.cb-yes, .cb-no {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 4px 10px;
		font-family: var(--font-ui);
		font-size: 12px;
		font-weight: 600;
		border-radius: 6px;
		cursor: pointer;
		white-space: nowrap;
		line-height: 1;
	}

	.cb-yes {
		color: var(--accent-text);
		background: var(--accent);
		border: 1px solid var(--accent);
	}
	.cb-yes.cb-danger {
		color: oklch(0.97 0 0);
		background: var(--error, #e05c5c);
		border-color: var(--error, #e05c5c);
	}

	.cb-no {
		color: var(--fg-muted);
		background: transparent;
		border: 1px solid var(--border);
	}
</style>
