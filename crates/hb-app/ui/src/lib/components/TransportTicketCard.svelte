<script lang="ts">
	// M18 W4 slice 2 — the asker's side of the fulfil flow: what a transport-ticket DM renders as.
	//
	// The THIRD structured render in chat (after W3's ShareCodeCard and W7.1b's ManifestFulfilCard),
	// and it follows the same rules: an ADDENDUM below the verbatim message text, never a replacement,
	// and ZERO action buttons inside the Q7 quarantine (Accept first, always).
	//
	// **This card reports; it does not gate.** Redemption is immediate and fires from the parent the
	// first time a ticket is seen — see `$lib/transport-ticket.ts` for why that is the asker's own
	// request completing rather than an auto-action, and why the backend has no deferred entry point
	// for a "Redeem later" button. The only button here is Retry, and only after a failure.
	//
	// It offers no way to fetch a FILE. What arrives is the collection's listing; the files stay with
	// their owner (INV-4′, MAS-INV-5) and no copy here suggests otherwise.
	import {
		REDEEMING_LINE,
		REDEEMED_LINE,
		REDEEM_FAILED_LINE,
		REDEEM_RETRY_LABEL,
		UNSOLICITED_LINE,
		UNVERIFIED_LINE,
		type RedemptionState,
	} from '$lib/transport-ticket.js';

	interface Props {
		/** The collection this ticket grants the listing for. */
		slug: string;
		/** The redemption's lifecycle state, owned by the parent's ledger. `undefined` while the
		 *  parent has not yet claimed it (a frame or two at most) — rendered as in-flight, because a
		 *  seen ticket is always about to be redeemed. */
		state: RedemptionState | undefined;
		/** True inside the Q7 request inbox → the card renders for recognition with no actions. */
		quarantined: boolean;
		/** Fired on Retry. The parent re-claims through the ledger, which only permits a retry from a
		 *  `failed` state — so this cannot become a general redeem-whenever affordance. */
		onretry: () => void;
	}

	let { slug, state, quarantined, onretry }: Props = $props();
	let kind = $derived(state?.kind ?? 'redeeming');
</script>

<div class="tt-card" data-state={quarantined ? 'quarantine' : kind} data-slug={slug}>
	{#if quarantined}
		<!-- Quarantine: recognition only. Accepting the sender comes first, exactly as for a share
		     code or a manifest request — the ticket keeps until then, because it has no expiry. -->
		<div class="tt-card-inert">Accept first to act on this.</div>
	{:else if kind === 'unverified'}
		<!-- The ask trace could not be read, so solicited/unsolicited is unknown. Fail closed (nothing
		     dialled) but say it is our side and that it retries — accusing the sender here would be
		     wrong, and leaving it looking permanent would hide a recoverable local failure. -->
		<div class="tt-card-inert">{UNVERIFIED_LINE}</div>
	{:else if kind === 'unsolicited'}
		<!-- We have no local record of asking this peer for this collection, so nothing was dialled.
		     Redeeming reveals our address to the owner; doing that unprompted would let any contact
		     harvest it by dropping a ticket in our inbox. See `wasAsked`. -->
		<div class="tt-card-inert">{UNSOLICITED_LINE}</div>
	{:else if kind === 'redeeming'}
		<div class="tt-card-inert">{REDEEMING_LINE(slug)}</div>
	{:else if kind === 'done'}
		<div class="tt-card-done">{REDEEMED_LINE(slug)}</div>
	{:else}
		<div class="tt-card-inert">{REDEEM_FAILED_LINE}</div>
		{#if state?.kind === 'failed' && state.message}
			<div class="tt-card-detail">{state.message}</div>
		{/if}
		<div class="tt-card-actions">
			<button type="button" class="tt-card-action" onclick={onretry}>{REDEEM_RETRY_LABEL}</button>
		</div>
	{/if}
</div>

<style>
	/* Matches ManifestFulfilCard's sizing so the structured renders read as one family. */
	.tt-card {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 9px 12px;
		margin-top: 6px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 9px;
		font-size: 12px;
	}

	.tt-card-inert {
		font-size: 11.5px;
		color: var(--fg-muted);
	}

	.tt-card-done {
		font-size: 11.5px;
		color: var(--fg);
		font-weight: 600;
	}

	.tt-card-detail {
		font-size: 11px;
		color: var(--fg-muted);
		font-family: var(--font-mono);
		word-break: break-word;
	}

	.tt-card-actions {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.tt-card-action {
		align-self: flex-start;
		padding: 5px 12px;
		font-size: 11.5px;
		font-weight: 600;
		color: var(--fg);
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 6px;
		cursor: pointer;
		white-space: nowrap;
		line-height: 1;
	}
	.tt-card-action:hover { filter: brightness(1.08); }
</style>
