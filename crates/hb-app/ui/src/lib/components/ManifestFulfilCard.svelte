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
		MANIFEST_AUTO_SENT_LINE,
		type ManifestFulfilState,
	} from '$lib/manifest-fulfil.js';

	interface Props {
		/** The pure-derived state for this request (from `deriveManifestFulfil`). Carries the slug. */
		state: ManifestFulfilState;
		/** The snapshot fingerprint the asker saw (carried in the request as `fingerprint_seen`).
		 *  Rendered so the owner can recognise which version of the tree the browser saw. Empty when
		 *  the requester sent none. */
		fingerprintSeen: string;
	}

	let { state, fingerprintSeen }: Props = $props();

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
		<div class="mf-card-inert">{MANIFEST_AUTO_SENT_LINE}</div>
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
		<!-- Carrier 4 (QURATOR-79): the third-party-author ask. WHAT is sent differs from the
		     own-collection case — a cached copy someone else authored, not a manifest built from this
		     owner's draft — so the line still names the author. There is no click: QURATOR-164
		     deleted approvals, and the auto-approve loop answers this without a human. -->
		<div class="mf-card-note" role="note">{MANIFEST_RESERVE_LINE(state.authorNpub)}</div>
		<div class="mf-card-inert">{MANIFEST_AUTO_SENT_LINE}</div>
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



	/* `.mf-card-action-secondary` and `.mf-card-secondary` were deleted with the markup they styled
	   (the Export button and the three hedging lines under it, removed on the owner's 2026-08-27
	   ruling). Component-scoped CSS makes an orphaned rule a Svelte warning, so they go together. */
</style>
