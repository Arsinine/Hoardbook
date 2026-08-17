<script lang="ts">
	// Chip tag editor for a collection's freeform tags (M13 W5 Slice 1) — mirrors the existing
	// profile-tags chip pattern in routes/+page.svelte (Enter/comma adds, Backspace removes the last),
	// componentized so the Add-collection wizard and Edit-details reopen can share it.
	interface Props {
		tags?: string[];
	}

	import { MAX_TAGS, MAX_TAG_CHARS } from '../limits.js';

	let { tags = $bindable([]) }: Props = $props();

	let input = $state('');

	// Tags ride in the published listing envelope, which shares one 40 KB budget with the folder
	// tree — the backend clamps to these same ceilings on save, so refusing here is what keeps the
	// user from typing something that silently disappears. MAX_TAGS is also a discovery choice:
	// the byte budget would tolerate many more.
	let atLimit = $derived(tags.length >= MAX_TAGS);

	function commit(next: string[]) {
		tags = next;
	}

	function addTag(raw: string) {
		const t = raw.trim().replace(/,$/, '').toLowerCase().slice(0, MAX_TAG_CHARS);
		if (t && !tags.includes(t) && !atLimit) commit([...tags, t]);
		input = '';
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ',') {
			e.preventDefault();
			addTag(input);
		} else if (e.key === 'Backspace' && !input && tags.length > 0) {
			commit(tags.slice(0, -1));
		}
	}

	function removeTag(i: number) {
		commit(tags.filter((_, idx) => idx !== i));
	}
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="hb-input tag-wrap" onclick={(e) => { if (e.target === e.currentTarget) e.currentTarget.querySelector('input')?.focus(); }}>
	{#each tags as tag, i (tag)}
		<span class="chip">
			{tag}
			<button type="button" class="chip-x" onclick={() => removeTag(i)} aria-label={`Remove ${tag}`}>×</button>
		</span>
	{/each}
	<input
		class="tag-input"
		type="text"
		maxlength={MAX_TAG_CHARS}
		placeholder={atLimit ? `${MAX_TAGS} tags max` : '+ add a tag'}
		disabled={atLimit}
		bind:value={input}
		onkeydown={handleKeydown}
	/>
</div>

<style>
	/* QURATOR-101 — on the .hb-input contract; height is overridden to auto (like .hb-textarea) so
	   the box can grow as chips wrap. :focus-within stands in for :focus since DOM focus lands on
	   the nested .tag-input, not this wrapper. */
	.tag-wrap {
		flex-wrap: wrap;
		gap: 5px;
		height: auto;
		min-height: 34px;
		padding: 5px 8px;
		cursor: text;
	}
	.tag-wrap:focus-within { border-color: var(--accent); }

	.chip {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 4px;
		padding: 1px 5px 1px 7px;
		font-size: 11.5px;
		color: var(--fg);
		white-space: nowrap;
	}

	.chip-x {
		background: none;
		border: none;
		cursor: pointer;
		color: var(--fg-dim);
		font-size: 14px;
		line-height: 1;
		padding: 0;
		display: flex;
		align-items: center;
	}
	.chip-x:hover { color: var(--fg); }

	.tag-input {
		flex: 1;
		min-width: 60px;
		background: transparent;
		border: none;
		outline: none;
		padding: 0;
	}
	.tag-input::placeholder { color: var(--fg-dim); }
</style>
