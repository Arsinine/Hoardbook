<script lang="ts">
	// Hoardbook Topics draft r1 — the roster row: avatar + display name + coloured fingerprint +
	// presence dot, replacing the roster's plain-text list. Visual precedent is Browse's People row
	// (`routes/browse/+page.svelte` `.contact-row`: avatar-wrap with the bottom-right `.online-dot`,
	// name, trailing meta — CSS at ~1292-1356 there). Deliberately NOT a shared component extracted
	// from it: Browse's row carries drag-to-group / multi-select wiring a roster member doesn't have
	// (a roster npub may not even be a saved contact), and its exact markup is pinned by
	// `routes/browse/browse-m23-w5.test.ts`. Same look, Topics-scoped.
	//
	// "Absent gracefully" (the M21 W4 behaviour-4 precedent: "no ring, no word row"): a roster npub
	// is NOT guaranteed to be a saved contact, and even a saved CachedPeer only carries `.fingerprint`
	// once resolved. The fingerprint has ONE implementation, in Rust — this only COLOURS the words it
	// was given (lib/fingerprint-colors.ts); it never derives one, so no contact / no fingerprint / a
	// self row renders avatar + name + presence only. Same for presence: no CachedPeer means no
	// `online` to read, so the dot is omitted entirely rather than guessed.
	import Avatar from './Avatar.svelte';
	import { fingerprintWordColor } from '$lib/fingerprint-colors.js';
	import { avatarHue } from '$lib/icons.js';

	interface Props {
		/** Display name — already resolved by `rosterLabel` (self/petname/short-npub fallback). */
		name: string;
		/** The avatar's letter (first character of the resolved name, uppercased by the caller). */
		letter: string;
		/** Optional avatar as a `data:` URI — passed through to <Avatar> when the contact has one. */
		picture?: string;
		/** The resolved §7 fingerprint (words picked by Rust), or undefined when not resolvable. */
		fingerprint?: { words: string[]; colorHex: string };
		/** True only when presence is KNOWN online (a saved contact's CachedPeer.online, or "you"). */
		online?: boolean;
	}

	let { name, letter, picture = undefined, fingerprint = undefined, online = false }: Props = $props();
	let hue = $derived(avatarHue(letter));
</script>

<div class="person-row">
	<div class="avatar-wrap">
		<Avatar {letter} size={24} {hue} {picture} />
		{#if online}
			<span class="online-dot"></span>
		{/if}
	</div>
	<div class="person-info">
		<div class="person-name">{name}</div>
		{#if fingerprint}
			<div class="fp-row">
				{#each fingerprint.words as w, i}
					{#if i > 0}<span class="fp-sep">·</span>{/if}
					<span class="fp-word" style={fingerprintWordColor(w) ? `color:${fingerprintWordColor(w)}` : undefined}>{w}</span>
				{/each}
			</div>
		{/if}
	</div>
</div>

<style>
	.person-row {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 5px 4px;
		border-radius: 5px;
		font-size: 12px;
	}
	.avatar-wrap { position: relative; flex-shrink: 0; }
	.online-dot {
		position: absolute;
		bottom: -1px;
		right: -1px;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--online);
		border: 1.5px solid var(--bg);
	}
	.person-info { min-width: 0; display: flex; flex-direction: column; gap: 1px; }
	.person-name {
		font-size: 12px;
		font-weight: 500;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.fp-row { display: flex; gap: 4px; align-items: baseline; flex-wrap: wrap; }
	.fp-word { font-family: var(--font-mono); font-size: 11px; font-weight: 600; }
	.fp-sep { color: var(--fg-dim); font-size: 11px; }
</style>
