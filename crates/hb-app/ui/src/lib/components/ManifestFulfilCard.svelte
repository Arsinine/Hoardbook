<script lang="ts">
	// M17 W7.1b — the manifest-request fulfilment card (the SECOND structured render in chat, after
	// W3's ShareCodeCard). The capability (`export_manifest`) already exists; this is its second entry
	// point — surfaced exactly where the request lands, instead of a submenu on a different tab.
	//
	// Same addendum + quarantine rules as W3:
	//   - renders as an ADDENDUM below the verbatim message text (never a replacement).
	//   - ZERO action buttons inside the Q7 request inbox (Accept first, always).
	//   - the export Tauri call fires ONLY on the click handler in the parent (zero network on render).
	//
	// The card's state is derived PURELY upstream (`deriveManifestFulfil` in $lib/manifest-fulfil.js)
	// so this component is a thin presentational shell — it renders exactly the state it's handed.
	import {
		MANIFEST_PRIVATE_LINE,
		MANIFEST_EMPTY_LINE,
		MANIFEST_MISSING_LINE,
		MANIFEST_STALE_NOTE,
		MANIFEST_RESERVE_LINE,
		type ManifestFulfilState,
	} from '$lib/manifest-fulfil.js';
	import { SEND_FULL_LIST_LABEL } from '$lib/transport-ticket.js';

	interface Props {
		/** The pure-derived state for this request (from `deriveManifestFulfil`). Carries the slug. */
		state: ManifestFulfilState;
		/** Fired on "Send the full list" (M18 W4) — the parent invokes `send_full_list`, which builds
		 *  the manifest, proves it fits the transport ceiling, mints a ticket for this one approval,
		 *  and DMs it. **Always behind this explicit click**: the app never auto-sends (M17 #4). */
		onsend: (slug: string) => void;
		/** Carrier 4 (QURATOR-79) — the re-serve verb, fired only from the `re-serve` state's button.
		 *  Same explicit click, same never-auto rule; the parent invokes `send_cached_manifest`,
		 *  which re-serves the cached envelope the author field names rather than building one. The
		 *  two verbs are separate props (not one re-dispatching closure) so each stays a single,
		 *  pin-able wiring in the route. */
		onserve: (state: ManifestFulfilState) => void;
		/** True while a send for this slug is in flight — the button disables rather than queueing a
		 *  second approval for the same request. */
		sending: boolean;
		/** The snapshot fingerprint the asker saw (carried in the request as `fingerprint_seen`).
		 *  Rendered so the owner can recognise which version of the tree the browser saw. Empty when
		 *  the requester sent none. */
		fingerprintSeen: string;
	}

	let { state, fingerprintSeen, onsend, onserve, sending }: Props = $props();

	let slug = $derived(state.slug);
</script>

<div class="mf-card" data-state={state.kind} data-slug={slug}>
	<div class="mf-card-head">
		<span class="mf-card-slug">{slug}</span>
		{#if fingerprintSeen}
			<span class="mf-card-fp" title="The tree snapshot the asker saw when they requested">{fingerprintSeen}</span>
		{/if}
	</div>

	{#if state.kind === 'public'}
		{#if state.stale}
			<div class="mf-card-note" role="note">{MANIFEST_STALE_NOTE}</div>
		{/if}
		<!-- Devtest 2026-08-27, owner: the card was carrying one action and three lines of hedging
		     under it. "Export manifest…" is gone, and so is the copy that pointed at it or at a big
		     relay — the owner's reasoning: "the entire purpose of this IS so you dont have to
		     manually find a third party service to send your files, but now we've put an artificial
		     limit on it the export manifest button just looks ridiculous". The M18 W4 note this
		     replaces argued export was a needed second route when the transport fails; that argument
		     is now the owner's to overrule, and they have. Export still exists on Home → ⋯ → Export,
		     which is where the over-the-ceiling error already sends people. -->
		<div class="mf-card-actions">
			<button
				type="button"
				class="btn-primary btn-sm mf-card-action"
				onclick={() => onsend(slug)}
				disabled={sending}
			>
				{sending ? '…' : SEND_FULL_LIST_LABEL}
			</button>
		</div>
	{:else if state.kind === 'private'}
		<!-- No big-relay hint here: a Private collection is sealed per recipient, so publishing to a
		     big relay would not get this asker anything. Pointing them at Settings would be advice
		     that cannot work — the inert line is the whole honest answer. -->
		<div class="mf-card-inert">{MANIFEST_PRIVATE_LINE}</div>
	{:else if state.kind === 'empty'}
		<div class="mf-card-inert">{MANIFEST_EMPTY_LINE}</div>
	{:else if state.kind === 'missing'}
		<div class="mf-card-inert">{MANIFEST_MISSING_LINE(slug)}</div>
	{:else if state.kind === 'quarantine'}
		<!-- Quarantine (Q7 request inbox): the card renders for recognition, but ZERO action buttons —
		     Accept comes first, always (hard constraint #3, same rule as W3's ShareCodeCard). -->
		<div class="mf-card-inert">Accept first to act on this request.</div>
	{:else if state.kind === 're-serve'}
		<!-- Carrier 4 (QURATOR-79): the third-party-author ask. The verb is the same explicit click,
		     but WHAT it sends is not: a cached copy someone else authored, not a manifest built from
		     this owner's draft. The line names the author so the click is an informed one. -->
		<div class="mf-card-note" role="note">{MANIFEST_RESERVE_LINE(state.authorNpub)}</div>
		<div class="mf-card-actions">
			<button
				type="button"
				class="btn-primary btn-sm mf-card-action"
				onclick={() => onserve(state)}
				disabled={sending}
			>
				{sending ? '…' : SEND_FULL_LIST_LABEL}
			</button>
		</div>
	{/if}
</div>

<style>
	/* Avatar-less compact block — the card is an addendum below the verbatim message text.
	   Matches ShareCodeCard's sizing so the two structured renders read as one family. */
	.mf-card {
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

	.mf-card-head {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}

	.mf-card-slug {
		font-family: var(--font-mono);
		font-size: 11.5px;
		font-weight: 600;
		color: var(--fg);
		min-width: 0;
	}

	.mf-card-fp {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		flex: 1;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.mf-card-note {
		font-size: 11px;
		color: var(--fg-muted);
		font-style: italic;
	}

	.mf-card-inert {
		font-size: 11.5px;
		color: var(--fg-muted);
	}

	.mf-card-actions {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	/* QURATOR-101: role now comes from .btn-primary/.btn-default btn-sm (app.css); these local
	   classes are the test hooks + the role-preserving hover/disabled nuances that differ from the
	   contract's defaults. */
	.mf-card-action { align-self: flex-start; }
	.mf-card-action:hover { filter: brightness(1.05); }
	.mf-card-action:disabled { opacity: 0.6; cursor: default; }

	/* `.mf-card-action-secondary` and `.mf-card-secondary` were deleted with the markup they styled
	   (the Export button and the three hedging lines under it, removed on the owner's 2026-08-27
	   ruling). Component-scoped CSS makes an orphaned rule a Svelte warning, so they go together. */
</style>
