<script lang="ts">
	// QURATOR-351 slice 5 (draft r5, states a/b/c) — the Discover view: the phonebook.
	// ONE front door (r5): a title, a tag, or a name → people. State a = the book (contacts ∪
	// relay teasers, size-first); state b = search in two groups (Has it / Looking for it, an
	// entry can be in both); state c = the person panel, with Looking for FIRST (the demand side
	// is why you opened the entry). Message is NEVER gated on being a contact (owner ruling);
	// an added stranger flips to Contact AT ONCE (owner ruling).
	// QURATOR-360 F1: the likes panel folded into this view (the old lane-C component deleted).
	import { goto } from '$app/navigation';
	import { discoverPeople, follow } from '$lib/api.js';
	import type { PhonebookEntry } from '$lib/types.js';
	import { profile as profileStore, toast } from '$lib/stores.js';
	import Avatar from './Avatar.svelte';
	import EmptyState from './EmptyState.svelte';
	import Modal from './Modal.svelte';
	import { fmtBytes, sortEntries, splitResults, highlight } from '$lib/discover-book.js';

	let entries = $state<PhonebookEntry[]>([]);
	let capped = $state(false);
	let coldStart = $state(false);
	let loading = $state(true);
	let error = $state(false);
	let query = $state('');
	let debounced = $state('');
	// Native <select> bind gives a string; keep the state a string and derive the union below
	// (svelte-check would flag assigning a raw string to a 'size' | 'likeness' state).
	let sortSel = $state('size');
	let interest = $state('All');
	let openNpub = $state<string | null>(null);
	let adding = $state<string[]>([]);
	// Owner ruling: an added person leaves the stranger state AT ONCE — flip the chip locally
	// without waiting for the next book load.
	let addedLocally = $state<Record<string, boolean>>({});
	let searchEl = $state<HTMLInputElement | undefined>(undefined);

	const searching = $derived(debounced.trim().length > 0);
	const sortMode = $derived(sortSel === 'likeness' ? ('likeness' as const) : ('size' as const));
	const myTotal = $derived($profileStore?.total_bytes ?? 0);
	const allTags = $derived(
		[...new Set(entries.flatMap((e) => e.tags))].sort((a, b) => a.localeCompare(b)),
	);
	const ordered = $derived(sortEntries(entries, sortMode));
	const bookEntries = $derived(
		interest === 'All' ? ordered : ordered.filter((e) => e.tags.includes(interest)),
	);
	const results = $derived(searching ? splitResults(entries) : null);
	const panelEntry = $derived(entries.find((e) => e.npub === openNpub) ?? null);
	const bookEmpty = $derived(!searching && !loading && !error && entries.length === 0);

	let fetchSeq = 0;
	async function fetchInto(q: string | null) {
		const seq = ++fetchSeq;
		loading = true;
		error = false;
		try {
			const res = await discoverPeople(q ?? undefined);
			if (seq !== fetchSeq) return; // a newer query superseded this one
			entries = res.entries;
			capped = res.capped;
			coldStart = res.cold_start;
		} catch (e) {
			if (seq !== fetchSeq) return;
			error = true;
		} finally {
			if (seq === fetchSeq) loading = false;
		}
	}

	// State a ⇄ b: the query debounces 250 ms; clearing it returns to the book.
	$effect(() => {
		const q = query.trim();
		const t = setTimeout(() => {
			debounced = q;
		}, 250);
		return () => clearTimeout(t);
	});
	$effect(() => {
		void fetchInto(searching ? debounced : null);
	});

	function onWindowKeydown(e: KeyboardEvent) {
		// `/` focuses the front-door search — but never steals a keystroke from another field
		// (same typing-target guard shape the contacts page uses for its window keydown).
		if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return;
		const t = (e.composedPath()[0] ?? e.target) as HTMLElement | null;
		if (
			t instanceof HTMLElement &&
			(t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)
		) {
			return;
		}
		e.preventDefault();
		searchEl?.focus();
	}

	function isContact(e: PhonebookEntry): boolean {
		return e.is_contact || Boolean(addedLocally[e.npub]);
	}

	async function addPerson(npub: string) {
		if (adding.includes(npub)) return;
		adding = [...adding, npub];
		try {
			await follow(npub);
			addedLocally[npub] = true;
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			adding = adding.filter((n) => n !== npub);
		}
	}

	function message(e: PhonebookEntry) {
		// The compose route the old discovery hit-cards used (works for strangers). M17 W2: a
		// discovery hit is keyless by design, so a STRANGER's Message always carries the ask-access
		// prefill; a contact gets the plain compose box.
		const intent = isContact(e) ? '' : '&intent=ask-access';
		void goto('/chat?compose=' + encodeURIComponent(e.npub) + intent);
	}

	const onlyTitleMatch = (e: PhonebookEntry) => e.matched.length === 1 && e.matched[0] === 'title';
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#snippet entryCard(e: PhonebookEntry)}
	<article
		class="entry"
		data-npub={e.npub}
		role="button"
		tabindex="0"
		onclick={() => (openNpub = e.npub)}
		onkeydown={(ev) => {
			if (ev.key === 'Enter') openNpub = e.npub;
		}}
	>
		<div class="e-top">
			<Avatar letter={(e.display_name.charAt(0) || '?').toUpperCase()} size={36} picture={e.picture} />
			<div class="e-id">
				<div class="e-name">
					<span class="e-name-text">
						{#each highlight(e.display_name, debounced) as seg}{#if seg.hit}<mark class="hit">{seg.text}</mark>{:else}{seg.text}{/if}{/each}
					</span>
					{#if isContact(e)}<span class="contact-chip">Contact</span>{/if}
					{#if searching && onlyTitleMatch(e)}<span class="title-chip" title="Matched a cached listing title">title</span>{/if}
				</div>
				<div class="e-meta">
					<b>{fmtBytes(e.total_bytes)}</b>{#if e.collections.length > 0} · {e.collections.length} collection{e.collections.length === 1 ? '' : 's'}{/if}{#if e.likeness != null} · {e.likeness}% like you{/if}
				</div>
			</div>
			<!-- stopPropagation: the card click opens the panel; its buttons must not -->
			<div
				class="e-acts"
				onclick={(ev) => ev.stopPropagation()}
				onkeydown={(ev) => ev.stopPropagation()}
				role="presentation"
			>
				{#if !isContact(e)}
					<button
						type="button"
						class="btn-default btn-xs"
						aria-label="Add to contacts"
						disabled={adding.includes(e.npub)}
						onclick={() => addPerson(e.npub)}>+</button
					>
				{/if}
				<button type="button" class="btn-ghost btn-xs" onclick={() => message(e)}>Message</button>
			</div>
		</div>
		{#if e.collections.length > 0}
			<div class="e-has">
				{#each e.collections as c, i}{#if i > 0} · {/if}{#each highlight(c.name, debounced) as seg}{#if seg.hit}<mark class="hit">{seg.text}</mark>{:else}{seg.text}{/if}{/each}{/each}
			</div>
		{/if}
		{#if e.looking_for}
			<div class="e-wants"><span class="wants-label">Looking for </span>{#each highlight(e.looking_for, debounced) as seg}{#if seg.hit}<mark class="hit">{seg.text}</mark>{:else}{seg.text}{/if}{/each}</div>
		{/if}
	</article>
{/snippet}

<div class="discover-book">
	<div class="tools-line">
		<label class="hb-input discover-search">
			<input
				bind:this={searchEl}
				bind:value={query}
				placeholder="A title, a tag, or a name"
				aria-label="Search the phonebook"
			/>
		</label>
		{#if searching}
			<span class="quiet">
				{results ? results.hasIt.length + results.lookingFor.length : 0} people match “{debounced}”.
			</span>
			<button type="button" class="btn-ghost btn-xs" onclick={() => (query = '')}>Clear ✕</button>
		{:else}
			<span class="quiet">Everyone on your relays, biggest hoard first.</span>
			<select class="hb-input tool-select" bind:value={sortSel} aria-label="Sort">
				<option value="size">Sort · Size</option>
				<option value="likeness">Sort · Likeness</option>
			</select>
			<select class="hb-input tool-select" bind:value={interest} aria-label="Interest">
				<option value="All">Interest · All</option>
				{#each allTags as tag (tag)}
					<option value={tag}>Interest · {tag}</option>
				{/each}
			</select>
		{/if}
	</div>

	{#if coldStart}
		<p class="note">Add Interests or a collection on Home to see how like you people are.</p>
	{/if}
	{#if capped}
		<p class="note quiet">Showing the first {entries.length}.</p>
	{/if}

	{#if error}
		<EmptyState
			error
			message="Couldn't load the book — the phonebook didn't answer."
			onretry={() => void fetchInto(searching ? debounced : null)}
		/>
	{:else if searching && loading}
		<p class="note quiet">Searching…</p>
	{:else if searching && results}
		<!-- Gated on !loading above: `entries` still holds the previous book (empty `matched`)
		     until the query's fetch lands, so splitting it would flash "Nothing matched". -->
		{#if results.hasIt.length === 0 && results.lookingFor.length === 0}
			<div class="empty-book"><p class="empty-title">Nothing matched "{debounced}".</p></div>
		{/if}
		{#if results.hasIt.length > 0}
			<div class="res-head">
				<h3 class="res-title">Has it</h3>
				<span class="quiet">{results.hasIt.length} people · biggest first</span>
			</div>
			<div class="book-grid">
				{#each results.hasIt as e (e.npub)}{@render entryCard(e)}{/each}
			</div>
		{/if}
		{#if results.lookingFor.length > 0}
			<div class="res-head">
				<h3 class="res-title">Looking for it</h3>
				<span class="quiet">{results.lookingFor.length} people</span>
			</div>
			<div class="book-grid">
				{#each results.lookingFor as e (e.npub)}{@render entryCard(e)}{/each}
			</div>
		{/if}
	{:else if loading}
		<p class="note quiet">Loading…</p>
	{:else if bookEmpty}
		<div class="empty-book">
			<p class="empty-title">No one in the book yet.</p>
			<p class="quiet">The book fills from the public teasers people publish on your relays.</p>
		</div>
	{:else}
		<div class="book-grid">
			{#each bookEntries as e (e.npub)}{@render entryCard(e)}{/each}
		</div>
	{/if}
</div>

<Modal
	open={!!panelEntry}
	onclose={() => (openNpub = null)}
	ariaLabel={panelEntry ? panelEntry.display_name + ' — person panel' : 'Person panel'}
	width="480px"
>
	{#if panelEntry}
		<div class="d-head">
			<Avatar letter={(panelEntry.display_name.charAt(0) || '?').toUpperCase()} size={48} picture={panelEntry.picture} />
			<div>
				<div class="d-name">{panelEntry.display_name}</div>
				<div class="quiet">
					{fmtBytes(panelEntry.total_bytes)}{#if panelEntry.likeness != null} · {panelEntry.likeness}% like you{/if}
				</div>
			</div>
		</div>
		<div class="d-actions">
			{#if !isContact(panelEntry)}
				<button
					type="button"
					class="btn-default btn-xs"
					aria-label="Add to contacts"
					disabled={adding.includes(panelEntry.npub)}
					onclick={() => addPerson(panelEntry.npub)}>+ Add</button
				>
			{/if}
			<button type="button" class="btn-default" onclick={() => message(panelEntry)}>Message</button>
		</div>
		<!-- Order is the ruling: Looking for FIRST, then Public collections, Interests, Contact
		     (LAST, only when the entry carries a hint). Topics is NOT sourceable — topic
		     membership is members-only/encrypted (hb-net/src/topic.rs), the teaser never
		     carries it. -->
		{#if panelEntry.looking_for}
			<div class="d-sec">Looking for</div>
			<p class="d-wants">{panelEntry.looking_for}</p>
		{/if}
		<div class="d-sec">Public collections</div>
		{#if panelEntry.collections.length > 0}
			<div class="coll">
				{#each panelEntry.collections as c (c.name)}
					<div class="coll-row"><span class="coll-name">{c.name}</span><span class="coll-size">{fmtBytes(c.bytes)}</span></div>
				{/each}
			</div>
		{:else}
			<p class="quiet">No public collections.</p>
		{/if}
		{#if panelEntry.total_bytes > myTotal}
			<!-- v5 size rule — exact ruling copy "Readable once your hoard reaches X". -->
			<p class="d-reason">
				Readable once your hoard reaches {fmtBytes(panelEntry.total_bytes)}.{#if myTotal > 0} Yours is {fmtBytes(myTotal)}.{/if}
			</p>
		{/if}
		{#if panelEntry.tags.length > 0}
			<div class="d-sec">Interests</div>
			<div class="chips">
				{#each panelEntry.tags as tag (tag)}<span class="chip">{tag}</span>{/each}
			</div>
		{/if}
		{#if panelEntry.contact_hint}
			<div class="d-sec">Contact</div>
			<p class="d-wants">{panelEntry.contact_hint}</p>
		{/if}
	{/if}
</Modal>

<style>
	.discover-book {
		display: flex;
		flex-direction: column;
		gap: 10px;
		min-width: 0;
	}
	.tools-line {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
	}
	.discover-search {
		flex: 1 1 240px;
		min-width: 200px;
	}
	.tool-select {
		height: 28px;
		font-size: 12px;
		width: auto;
	}
	.quiet {
		color: var(--fg-dim, var(--fg-muted, inherit));
		font-size: 12px;
	}
	.note {
		font-size: 12.5px;
		color: var(--fg-muted, inherit);
		margin: 0;
	}
	.book-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
		gap: var(--panel-gap, 10px);
	}
	.entry {
		border: 1px solid var(--border, transparent);
		border-radius: var(--r-card, 12px);
		padding: 10px 12px;
		display: flex;
		flex-direction: column;
		gap: 6px;
		cursor: pointer;
		min-width: 0;
	}
	.entry:hover {
		border-color: var(--accent, currentColor);
	}
	.e-top {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.e-id {
		flex: 1;
		min-width: 0;
	}
	.e-name {
		font-size: 13px;
		font-weight: 600;
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
	}
	.e-name-text {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.contact-chip,
	.title-chip {
		font-size: 9.5px;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--fg-muted, inherit);
		border: 1px solid var(--border, currentColor);
		border-radius: var(--r-chip, 999px);
		padding: 0 5px;
		flex: none;
	}
	.title-chip {
		font-weight: 400;
		text-transform: none;
		font-style: italic;
	}
	.e-meta {
		font-size: 11.5px;
		color: var(--fg-muted, inherit);
	}
	.e-has,
	.e-wants {
		font-size: 11.5px;
		color: var(--fg-muted, inherit);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.e-has {
		border-top: 1px solid var(--divider, var(--border, transparent));
		padding-top: 6px;
	}
	.e-wants {
		color: var(--fg-dim, var(--fg-muted, inherit));
		font-style: italic;
	}
	.wants-label {
		font-style: normal;
		font-weight: 600;
	}
	.e-acts {
		display: flex;
		gap: 6px;
		flex: none;
	}
	.res-head {
		display: flex;
		align-items: baseline;
		gap: 8px;
		margin-top: 4px;
	}
	.res-title {
		font-size: 13px;
		margin: 0;
	}
	mark.hit,
	.hit {
		background: none;
		color: var(--accent, currentColor);
		font-style: normal;
	}
	.empty-book {
		text-align: center;
		padding: 32px 12px;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.empty-title {
		font-weight: 600;
		margin: 0;
	}
	.empty-book .quiet {
		font-size: 12.5px;
	}
	.d-head {
		display: flex;
		gap: 10px;
		align-items: center;
	}
	.d-name {
		font-weight: 600;
		font-size: 15px;
	}
	.d-actions {
		display: flex;
		gap: 8px;
		margin: 10px 0 2px;
	}
	.d-sec {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--fg-dim, var(--fg-muted, inherit));
		margin-top: 12px;
	}
	.d-wants {
		margin: 4px 0 0;
		font-size: 13px;
	}
	.coll {
		display: flex;
		flex-direction: column;
	}
	.coll-row {
		display: flex;
		justify-content: space-between;
		gap: 8px;
		font-size: 12.5px;
		padding: 3px 0;
	}
	.coll-size {
		color: var(--fg-dim, inherit);
	}
	.d-reason {
		font-size: 12px;
		color: var(--fg-dim, inherit);
		margin: 6px 0 0;
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}
	.chip {
		font-size: 11px;
		border: 1px solid var(--border, currentColor);
		border-radius: var(--r-chip, 999px);
		padding: 1px 8px;
		color: var(--fg-muted, inherit);
	}
</style>
