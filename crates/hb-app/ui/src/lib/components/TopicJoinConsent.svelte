<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { joinConsentCopy, NO_UNLOCK_NOTE } from '../topics-view.js';

	/** Whether the Topic being joined is private (durable members-only record) or public. */
	export let isPrivate = false;
	/** Disable while a join is in flight. */
	export let disabled = false;

	/** F12 — the explicit acknowledgment. The Join button stays disabled until this is checked. */
	let acknowledged = false;

	const dispatch = createEventDispatcher<{ join: void; cancel: void }>();
	const ackId = `topic-ack-${Math.random().toString(36).slice(2, 9)}`;

	$: copy = joinConsentCopy(isPrivate);

	/** F12 — the join is a real logical gate, not just a disabled attribute: it cannot fire without the
	 *  explicit acknowledgment, even if a synthetic click reaches the (visually disabled) button. */
	function tryJoin() {
		if (disabled || !acknowledged) return;
		dispatch('join');
	}
</script>

<div class="join-consent">
	<p class="consent" role="note">{copy}</p>
	<p class="no-unlock">{NO_UNLOCK_NOTE}</p>

	<label class="ack" for={ackId}>
		<input id={ackId} type="checkbox" bind:checked={acknowledged} {disabled} />
		I understand and want to join.
	</label>

	<div class="actions">
		<button type="button" class="cancel" on:click={() => dispatch('cancel')}>Cancel</button>
		<button
			type="button"
			class="join"
			disabled={disabled || !acknowledged}
			on:click={tryJoin}
		>
			Join Topic
		</button>
	</div>
</div>

<style>
	.join-consent {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.consent {
		margin: 0;
		font-size: 12.5px;
		line-height: 1.45;
		color: var(--fg);
	}

	.no-unlock {
		margin: 0;
		font-size: 11.5px;
		line-height: 1.4;
		color: var(--fg-dim);
	}

	.ack {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 12.5px;
		color: var(--fg-muted);
	}

	.actions {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
	}

	.actions button {
		padding: 5px 12px;
		border-radius: 6px;
		border: 1px solid var(--border);
		font: inherit;
		cursor: pointer;
	}

	.actions .join:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
