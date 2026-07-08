<script lang="ts">
	// Chip tag editor for a collection's freeform tags (M13 W5 Slice 1) — mirrors the existing
	// profile-tags chip pattern in routes/+page.svelte (Enter/comma adds, Backspace removes the last),
	// componentized so the Add-collection wizard and Edit-details reopen can share it.
	interface Props {
		tags?: string[];
	}

	let { tags = $bindable([]) }: Props = $props();

	let input = $state('');

	function commit(next: string[]) {
		tags = next;
	}

	function addTag(raw: string) {
		const t = raw.trim().replace(/,$/, '').toLowerCase();
		if (t && !tags.includes(t)) commit([...tags, t]);
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
<div class="tag-wrap" onclick={(e) => { if (e.target === e.currentTarget) e.currentTarget.querySelector('input')?.focus(); }}>
	{#each tags as tag, i (tag)}
		<span class="chip">
			{tag}
			<button type="button" class="chip-x" onclick={() => removeTag(i)} aria-label={`Remove ${tag}`}>×</button>
		</span>
	{/each}
	<input
		class="tag-input"
		type="text"
		placeholder="+ add a tag"
		bind:value={input}
		onkeydown={handleKeydown}
	/>
</div>

<style>
	.tag-wrap {
		display: flex;
		flex-wrap: wrap;
		gap: 5px;
		min-height: 34px;
		padding: 5px 8px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
		align-items: center;
		cursor: text;
		transition: border-color 0.1s;
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
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		padding: 0;
	}
	.tag-input::placeholder { color: var(--fg-dim); }
</style>
