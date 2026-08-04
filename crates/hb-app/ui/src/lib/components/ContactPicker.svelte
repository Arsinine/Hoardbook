<script lang="ts">
	// M21 W3 — the one contact-picker used by two sites: Topics → Invite modal, and Chat → compose "+".
	// Presentational only: it owns the contact list + a free-npub field and calls `onselect` with the
	// chosen npub string. Each site wires that npub into its OWN existing path (topicInvite for Topics,
	// setting composeTo for Chat) — there is no second send route here. Single-select on both sites
	// because both backing commands take exactly one recipient (api.ts topicInvite; chat sendMessage),
	// and multi-select for Topics would mean looping topicInvite client-side with partial-failure UX
	// ("2 of 3 sent") that the owner did not ask for (CLAUDE.md §2 Simplicity First).
	import Modal from './Modal.svelte';
	import Avatar from './Avatar.svelte';
	import { avatarHue } from '$lib/icons.js';
	import { contactDisplayName, shortNpub } from '$lib/contact-display.js';
	import type { CachedPeer } from '../types.js';

	interface Props {
		open?: boolean;
		title?: string;
		/** Contacts to list. The caller passes the store value; the picker filters out the local user. */
		contacts: CachedPeer[];
		/** The local user's npub — excluded from the list (you can't invite/message yourself). */
		myNpub?: string;
		/** CTA label for the confirm control (e.g. "Invite", "Select"). */
		confirmLabel?: string;
		onselect?: (npub: string) => void;
		onclose?: () => void;
	}

	let {
		open = false,
		title = 'Choose a contact',
		contacts,
		myNpub = '',
		confirmLabel = 'Select',
		onselect,
		onclose,
	}: Props = $props();

	// The free-text npub/share-code field — the "enter a new npub" half of the owner's ask. Holds
	// whatever the user types (npub OR a contact's full share code), unchanged from the legacy inputs.
	let manualNpub = $state('');
	// Which contact row is highlighted (npub key). null = none selected → the manual field is the path.
	let selectedNpub = $state<string | null>(null);

	// Reset on every closed→open edge so a re-open starts clean (matches AddContactDialog's wasOpen
	// pattern: a transition-edge flag, not a reactive dependency).
	let wasOpen = false;
	$effect(() => {
		if (open && !wasOpen) {
			wasOpen = true;
			manualNpub = '';
			selectedNpub = null;
		} else if (!open && wasOpen) {
			wasOpen = false;
		}
	});

	// The local user never appears in their own picker (can't invite/message yourself). The contact
	// list is otherwise passed through as-is — no search/filter was requested (out of scope, and the
	// existing contact code does not filter here).
	let listed = $derived(myNpub ? contacts.filter((c) => c.npub !== myNpub) : contacts);

	let selectedPeer = $derived(selectedNpub ? listed.find((c) => c.npub === selectedNpub) ?? null : null);

	// The npub the confirm button will emit: a selected contact's npub, else the trimmed manual field.
	// A contact's FULL share code is reachable too — but selecting a contact means "this person", so we
	// emit their npub (the stable identity both commands accept). The manual field is the escape hatch
	// for a not-yet-a-contact npub/share-code.
	let chosen = $derived(selectedPeer ? selectedPeer.npub : manualNpub.trim());
	let canConfirm = $derived(chosen.length > 0);

	function confirm() {
		if (!canConfirm) return;
		const v = chosen;
		manualNpub = '';
		selectedNpub = null;
		onselect?.(v);
	}

	function cancel() {
		manualNpub = '';
		selectedNpub = null;
		onclose?.();
	}

	// Selecting a row clears the manual field; typing in the manual field clears the row selection —
	// the two paths are mutually exclusive (picking a contact means "this person", not "seed the box").
	function pickRow(npub: string) {
		selectedNpub = npub;
		manualNpub = '';
	}
	function onManualInput() {
		selectedNpub = null;
	}
</script>

<Modal {open} {title} level="stacked" onclose={cancel}>
	{#if listed.length > 0}
		<div class="picker-hint">Pick from your contacts, or type a new npub below.</div>
		<ul class="contact-list" role="listbox" aria-label="Contacts">
			{#each listed as peer (peer.npub)}
				{@const name = contactDisplayName(peer)}
				{@const initial = name[0]?.toUpperCase() ?? '?'}
				{@const hue = avatarHue(initial)}
				<li>
					<button
						type="button"
						class="contact-row"
						class:row-selected={selectedNpub === peer.npub}
						role="option"
						aria-selected={selectedNpub === peer.npub}
						onclick={() => pickRow(peer.npub)}
					>
						<Avatar letter={initial} size={30} {hue} picture={peer.profile?.picture} />
						<span class="row-name">{name}</span>
						<span class="row-npub">{shortNpub(peer.npub)}</span>
						{#if selectedNpub === peer.npub}<span class="row-check" aria-hidden="true">✓</span>{/if}
					</button>
				</li>
			{/each}
		</ul>
	{:else}
		<p class="picker-empty">You don’t have any contacts yet. Type an npub or share code below.</p>
	{/if}

	<div class="manual">
		<label for="cp-npub" class="manual-label">Or enter an npub / share code</label>
		<input
			id="cp-npub"
			class="hb-input"
			type="text"
			bind:value={manualNpub}
			oninput={onManualInput}
			placeholder="npub1… or hbk1…"
			onkeydown={(e) => e.key === 'Enter' && confirm()}
		/>
	</div>

	{#snippet actions()}
		<button type="button" class="btn-ghost" onclick={cancel}>Cancel</button>
		<button type="button" class="btn-primary" disabled={!canConfirm} onclick={confirm}>{confirmLabel}</button>
	{/snippet}
</Modal>

<style>
	.picker-hint { font-size: 11.5px; color: var(--fg-muted); margin-bottom: 8px; }
	.contact-list {
		list-style: none; margin: 0 0 12px; padding: 0;
		max-height: 240px; overflow-y: auto;
		display: flex; flex-direction: column; gap: 2px;
	}
	.contact-row {
		display: flex; align-items: center; gap: 9px; width: 100%;
		padding: 6px 8px; border: 1px solid transparent; border-radius: 7px;
		background: transparent; cursor: pointer; text-align: left;
		color: var(--fg); font: inherit; font-size: 12.5px;
	}
	.contact-row:hover { background: var(--bg-elev1); }
	.row-selected { border-color: var(--accent); background: var(--bg-elev1); }
	.row-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.row-npub { font-family: var(--font-mono); font-size: 10.5px; color: var(--fg-muted); flex-shrink: 0; }
	.row-check { display: inline-flex; color: var(--accent); flex-shrink: 0; }
	.picker-empty { font-size: 12.5px; color: var(--fg-muted); margin: 0 0 12px; }
	.manual { display: flex; flex-direction: column; gap: 5px; }
	.manual-label { font-size: 11px; color: var(--fg-muted); font-weight: 500; }
	/* M21 W3: the npub input uses the global .hb-input contract (app.css) — no input background here. */
</style>
