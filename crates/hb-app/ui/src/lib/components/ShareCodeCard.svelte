<script lang="ts">
	// M17 W3 — the received-share-code card (the consume leg). The ONE structured render in chat:
	// an avatar-less compact block showing the word+color fingerprint of the EMBEDDED npub (single-
	// sourced in hb-core via `share_code_info`, never re-derived in JS), the code type, and the
	// impersonation defenses that already exist in the add funnel (petname match / "different key"
	// collision badge). Detection + this card cost ZERO network at render time — the only Tauri
	// commands the render path invokes are `validate_share_code` + `share_code_info`, both local.
	// Resolution (`pasteKey` / `follow`) fires ONLY on the user's click (hard constraint #1/#5).
	import { renderFingerprint, petnameFor, strangerBadge, type Contact } from '$lib/identity-display.js';
	import type { CachedPeer } from '../types.js';
	import type { ShareCodeInfo } from '../api.js';

	interface Props {
		/** The locally-parsed share code info (npub + fingerprint + has_browse_key). From cache —
		 *  never fetched at render time. */
		info: ShareCodeInfo;
		/** The chat peer the bubble belongs to (null in the Q7 request inbox — quarantine state). */
		chatPeerNpub: string | null;
		/** The user's own npub (for the sent-bubble own-code inert state). */
		ownNpub: string;
		/** The user's contacts (for the impersonation collision badge + already-keyed check). */
		contacts: CachedPeer[];
		/** True inside the Q7 request inbox — the card renders visually but with NO action buttons
		 *  (the quarantine rule: Accept comes first, always). */
		quarantined: boolean;
		/** True once the user has clicked Unlock and the contact is now keyed (flips the card to the
		 *  inert "Unlocked ✓" state). The parent owns this so the card can't self-heal. */
		unlocked: boolean;
		/** Fired on "Unlock browsing" click — the parent runs `pasteKey` → `follow` (the only network
		 *  surface, behind an explicit click). */
		onunlock: () => void;
		/** Fired on "Add contact" click for a forwarded third-party code — the parent opens the
		 *  standard AddContactDialog (petname + group; a NEW relationship, full ritual applies). */
		onaddcontact: (info: ShareCodeInfo) => void;
	}

	let {
		info,
		chatPeerNpub,
		ownNpub,
		contacts,
		quarantined,
		unlocked,
		onunlock,
		onaddcontact,
	}: Props = $props();

	// The four card states (spec W3):
	//   own        — your own code in a sent bubble → inert informational "Your share code".
	//   same       — embedded npub == chat peer: Unlock (if keyless) or Already unlocked (if keyed).
	//   third      — embedded npub ≠ chat peer (forwarded): Add contact (full add funnel).
	//   quarantine — inside the Q7 request inbox: card renders, ZERO action buttons.
	let isOwn = $derived(info.npub === ownNpub);
	let isSamePeer = $derived(chatPeerNpub !== null && info.npub === chatPeerNpub);
	let existingContact = $derived(contacts.find((c) => c.npub === info.npub));
	// A contact is "already keyed" when the embedded code's browse-key flag matches a saved contact
	// who already holds a browse-key (i.e. they were added with a full hbk code, not a bare npub).
	let alreadyKeyed = $derived(
		isSamePeer && existingContact?.browse_key_hex != null && existingContact.browse_key_hex !== '',
	);
	// "Unlocked" wins over "already keyed" — covers the just-clicked session state.
	let state = $derived<'own' | 'same' | 'third' | 'quarantine'>(
		quarantined ? 'quarantine' : isOwn ? 'own' : isSamePeer ? 'same' : 'third',
	);

	// The impersonation distinguisher: the petname a user bound to THIS npub (verified) vs a name
	// reused under a different key (the "different key" collision badge). Reuses the existing helper
	// so the card never re-derives identity trust (it inherits the add funnel's defenses). Map
	// CachedPeer → the Contact shape petnameFor expects ({npub, petname}).
	let contactsForLabel = $derived<Contact[]>(
		contacts.map((c) => ({ npub: c.npub, petname: c.petname ?? c.profile?.display_name ?? '' })),
	);
	let displayName = $derived(existingContact?.profile?.display_name ?? existingContact?.petname ?? info.npub);
	let label = $derived(petnameFor(info.npub, displayName, contactsForLabel));
	let collision = $derived(strangerBadge(label));
	let codeTypeLabel = $derived(info.has_browse_key ? 'Share code' : 'npub');
</script>

<div class="share-card" data-state={state} data-npub={info.npub}>
	<div class="share-card-head">
		<span class="share-card-swatch" style="background:{info.fingerprint.colorHex}" aria-hidden="true"></span>
		<span class="share-card-fp">{renderFingerprint(info.fingerprint)}</span>
		<span class="share-card-type">{codeTypeLabel}</span>
	</div>
	{#if collision}
		<div class="share-card-collision" role="note">⚠ {collision}</div>
	{/if}

	{#if state === 'own'}
		<div class="share-card-inert">Your share code</div>
	{:else if state === 'quarantine'}
		<!-- Quarantine (Q7 request inbox): the card renders visually for recognition, but ZERO action
		     buttons — Accept comes first, always (hard constraint #3). -->
		<div class="share-card-inert">Accept to unlock</div>
	{:else if state === 'same'}
		{#if unlocked || alreadyKeyed}
			<div class="share-card-inert share-card-done">
				Unlocked ✓
				{#if unlocked}
					<a href="/browse?peer={info.npub}" class="share-card-link">Browse →</a>
				{/if}
			</div>
		{:else}
			<button type="button" class="btn-primary share-card-action" onclick={onunlock}>Unlock browsing</button>
		{/if}
	{:else if state === 'third'}
		<button type="button" class="btn-primary share-card-action" onclick={() => onaddcontact(info)}>Add contact</button>
	{/if}
</div>

<style>
	/* Avatar-less compact block — the card is an addendum below the verbatim message text. */
	.share-card {
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

	.share-card-head {
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.share-card-swatch {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		flex-shrink: 0;
		box-shadow: inset 0 0 0 1px oklch(1 0 0 / 0.12);
	}

	.share-card-fp {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-muted);
		flex: 1;
		min-width: 0;
	}

	.share-card-type {
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--fg-dim);
		padding: 1px 6px;
		border-radius: 999px;
		background: color-mix(in oklch, var(--fg-muted) 10%, transparent);
		flex-shrink: 0;
	}

	.share-card-collision {
		font-size: 11px;
		color: oklch(0.78 0.14 35);
	}

	.share-card-inert {
		font-size: 11.5px;
		color: var(--fg-muted);
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.share-card-done {
		color: var(--online, oklch(0.7 0.15 145));
		font-weight: 500;
	}

	.share-card-link {
		color: var(--accent);
		text-decoration: none;
		font-size: 11.5px;
		font-weight: 500;
	}
	.share-card-link:hover { text-decoration: underline; }

	.share-card-action {
		align-self: flex-start;
		padding: 5px 12px;
		font-size: 11.5px;
		font-weight: 600;
		color: var(--accent-text);
		background: var(--accent);
		border: 1px solid var(--accent);
		border-radius: 6px;
		cursor: pointer;
		white-space: nowrap;
		line-height: 1;
	}
	.share-card-action:hover { filter: brightness(1.05); }
	.share-card-action:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
