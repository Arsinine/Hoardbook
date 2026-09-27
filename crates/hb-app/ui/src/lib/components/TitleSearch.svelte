<script lang="ts">
	// QURATOR-342 D1 — the title-search strip for the top of Browse (lane D2 mounts it as
	// `<TitleSearch onbrowse={(npub) => …} />`). Self-fetches `searchTitles`, debounced 250 ms.
	//
	// Owner rulings bound here (QURATOR-342, 2026-09-27/28):
	// - Title-search holder rows stay BARE — no overlap/similarity text on a holder row.
	// - Title-search holders are always readable, so Browse is unconditional; locked strangers keep
	//   "+ Add" (adding is not reading).
	// - Copy bans: no "download", no "follow/following" (lib/copy-audit.ts).
	//
	// The honest limit — the hint under the input states it and the two foot lines (holder cap,
	// truncated titles) enforce it; a FAILED search must never look like "no results".
	import { searchTitles, follow, getContacts } from '../api.js';
	import type { TitleHit, TitleHolder } from '../types.js';
	import { contacts, loadContactsInto, toast } from '../stores.js';
	import { contactDisplayName, shortNpub } from '../contact-display.js';
	import Avatar from './Avatar.svelte';

	interface Props {
		onbrowse: (npub: string) => void;
	}
	let { onbrowse }: Props = $props();

	/** Owner ruling: at most 50 titles / 20 holders per answer — the backend enforces the caps, the
	 *  foot lines disclose them. `limit` here only pins the title cap explicitly. */
	const TITLE_LIMIT = 50;
	const DEBOUNCE_MS = 250;

	let query = $state('');
	// `null` = no search yet (empty input). Distinct from `done && !hits.length` (a real empty
	// answer) and from `error` — a failed search must never render as "no results".
	let status = $state<'idle' | 'loading' | 'done' | 'error'>('idle');
	let hits = $state<TitleHit[]>([]);
	let truncated = $state(false);
	let adding = $state<string | null>(null); // npub currently being added (disables its button)

	let timer: ReturnType<typeof setTimeout> | undefined;

	// Each search carries the sequence number it was launched under; a late answer from a superseded
	// search (slow relay, fast typing) is discarded instead of overwriting fresher results.
	let searchSeq = 0;

	function onInput(e: Event) {
		// Read from the event, not `query` — bind/listener ordering must never decide what we search.
		query = (e.currentTarget as HTMLInputElement).value;
		if (timer) clearTimeout(timer);
		const q = query.trim();
		if (!q) {
			// Empty query: NOTHING below the input but the honest-limit hint. Clears a pending debounce
			// and stale results/error so an old answer can't outlive its query.
			++searchSeq;
			hits = [];
			truncated = false;
			status = 'idle';
			return;
		}
		timer = setTimeout(() => void runSearch(q), DEBOUNCE_MS);
	}

	async function runSearch(q: string) {
		const seq = ++searchSeq;
		status = 'loading';
		try {
			const res = await searchTitles(q, TITLE_LIMIT);
			if (seq !== searchSeq) return; // superseded by a newer keystroke/retry — discard
			hits = res.hits;
			truncated = res.truncated;
			status = 'done';
		} catch {
			if (seq !== searchSeq) return;
			hits = [];
			truncated = false;
			status = 'error'; // error ≠ empty: the "no results" line must not show
		}
	}

	function retry() {
		const q = query.trim();
		if (!q) return;
		if (timer) clearTimeout(timer);
		void runSearch(q);
	}

	async function addHolder(h: TitleHolder) {
		if (adding) return;
		adding = h.npub;
		try {
			// The bare npub as the code — awareness, NOT a browse-key (INV-2); the same add path
			// AddContactPanel's followHit uses (npub as code, nothing pre-resolved).
			await follow(h.npub);
			// Refresh the store so the row flips to "Added ✓" and stays consistent everywhere.
			await loadContactsInto(getContacts);
			toast('Added to contacts');
		} catch {
			toast('Could not add this person', 'error');
		} finally {
			adding = null;
		}
	}

	// Petname (if a contact), then the holder's published display_name, then a short npub.
	function holderName(h: TitleHolder): string {
		const c = $contacts.find((x) => x.npub === h.npub);
		if (c) return contactDisplayName({ npub: c.npub, petname: c.petname, profile: c.profile });
		return h.display_name?.trim() || shortNpub(h.npub);
	}

	$effect(() => () => {
		if (timer) clearTimeout(timer); // unmount — never fire after the strip is gone
	});
</script>

<div class="ts-strip">
	<input
		class="hb-input ts-input"
		type="search"
		placeholder="Search titles…"
		aria-label="Search titles"
		value={query}
		oninput={onInput}
	/>
	<p class="ts-hint">Counts cover only people you can read.</p>

	{#if status === 'loading' && !hits.length}
		<p class="ts-line">Searching…</p>
	{/if}

	{#if status === 'error'}
		<p class="ts-line ts-error">
			Search failed.
			<button class="btn-default btn-sm" type="button" onclick={retry}>Retry</button>
	</p>
	{/if}

	{#if status === 'done' && !hits.length}
		<p class="ts-line">No titles match your search.</p>
	{/if}

	{#if hits.length}
		<div class="ts-results">
			{#each hits as hit}
				<details class="ts-result">
					<summary>
						<span class="ts-title">{hit.title}</span>
						<span class="ts-count">{hit.holder_count} {hit.holder_count === 1 ? 'person has' : 'people have'} this</span>
					</summary>
					<div class="ts-holders">
						{#each hit.holders as h}
							<div class="ts-holder">
								<Avatar letter={holderName(h).slice(0, 1)} size={24} />
								<span class="ts-name">{holderName(h)}</span>
								<span class="ts-path">{h.slug}/{h.path}</span>
								<span class="ts-actions">
									<button class="btn-default btn-xs" type="button" onclick={() => onbrowse(h.npub)}>Browse</button>
									{#if $contacts.some((c) => c.npub === h.npub)}
										<span class="ts-added">Added ✓</span>
									{:else}
										<button
											class="btn-default btn-xs"
											type="button"
											disabled={adding === h.npub}
											onclick={() => addHolder(h)}
										>
											{adding === h.npub ? 'Adding…' : '+ Add'}
										</button>
									{/if}
								</span>
							</div>
						{/each}
						{#if hit.holder_count > hit.holders.length}
							<p class="ts-foot">Showing first {hit.holders.length} of {hit.holder_count} holders</p>
						{/if}
					</div>
				</details>
			{/each}
			{#if truncated}
				<p class="ts-foot">{hits.length} titles shown — refine the search to see more</p>
			{/if}
		</div>
	{/if}
</div>

<style>
	.ts-strip {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.ts-input {
		width: 100%;
	}
	.ts-hint {
		margin: 0;
		font-size: 12px;
	 color: var(--fg-dim);
	}
	.ts-line {
		margin: 2px 0;
		font-size: 13px;
		color: var(--fg-dim);
	}
	.ts-error {
		color: var(--error);
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.ts-results {
		display: flex;
		flex-direction: column;
		border-top: 1px solid var(--border);
	}
	.ts-result {
		border-bottom: 1px solid var(--border);
	}
	.ts-result summary {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 7px 2px;
		cursor: pointer;
		list-style: none;
	}
	.ts-result summary::-webkit-details-marker {
		display: none;
	}
	.ts-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ts-count {
		flex-shrink: 0;
		margin-left: auto;
		font-size: 12px;
		color: var(--fg-dim);
	}
	.ts-holders {
		padding: 0 2px 8px 14px;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.ts-holder {
		display: flex;
		align-items: center;
		gap: 8px;
		min-height: 28px;
	}
	.ts-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ts-path {
		color: var(--fg-dim);
		font-size: 12px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ts-actions {
		flex-shrink: 0;
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.ts-added {
		font-size: 12px;
		color: var(--fg-dim);
	}
	.ts-foot {
		margin: 4px 0 0;
		font-size: 12px;
		color: var(--fg-dim);
	}
</style>
