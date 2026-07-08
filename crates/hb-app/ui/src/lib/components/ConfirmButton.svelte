<script lang="ts">
	// Shared inline two-step confirm (M13 W5) — mirrors the Settings "Wipe all data" pattern (a
	// boolean reveal → "Are you sure? …" + Confirm/Cancel), now reusable for every destructive action
	// (collection Remove, contact Remove, Topic Leave) instead of firing straight from a single click.

	interface Props {
		/** The resting trigger's label (e.g. "Remove", "Leave"). */
		label?: string;
		/** The revealed prompt text. */
		confirmText?: string;
		confirmLabel?: string;
		cancelLabel?: string;
		/** Styles the trigger + confirm button as destructive (red). */
		danger?: boolean;
		disabled?: boolean;
		onconfirm?: () => void;
		[key: string]: any
	}

	let {
		label = 'Remove',
		confirmText = 'Are you sure?',
		confirmLabel = 'Confirm',
		cancelLabel = 'Cancel',
		danger = true,
		disabled = false,
		onconfirm,
		...rest
	}: Props = $props();

	let revealed = $state(false);

	function start() {
		if (disabled) return;
		revealed = true;
	}
	function cancel() {
		revealed = false;
	}
	function confirm() {
		revealed = false;
		onconfirm?.();
	}
</script>

{#if !revealed}
	<button type="button" class="cb-trigger" class:cb-danger={danger} onclick={start} {disabled} {...rest}>
		{label}
	</button>
{:else}
	<span class="cb-confirm">
		<span class="cb-confirm-text">{confirmText}</span>
		<button type="button" class="cb-yes" class:cb-danger={danger} onclick={confirm}>{confirmLabel}</button>
		<button type="button" class="cb-no" onclick={cancel}>{cancelLabel}</button>
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
