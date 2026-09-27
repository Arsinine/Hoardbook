<script lang="ts">
	// QURATOR-342 Lane C — the "People like you" panel (replaces the orchestrator stub).
	// Lives in CONTACTS (lane B mounts `<PeopleLikeYou />`); no props, self-fetches
	// `similarPeople()` and reads the `contacts` store.
	//
	// Owner rulings bound here (2026-09-27/28):
	//   - Cards 3 across (a DOM-pinned inline style — jsdom computes no stylesheet layout, so the
	//     exactly-3-columns ruling is asserted on the style ATTRIBUTE, not a scoped class rule).
	//   - "their profile appears when you hover over" a card: hover OR keyboard focus opens a
	//     role=tooltip popover from the contact's Profile; Escape/blur/mouseleave dismiss.
	//   - No pills, no Ask-for-access button, no % score, no "download", no "follow", no
	//     "share(s)" outside the share-code sense (lib/copy-audit.ts) — the interests reason line
	//     is therefore "In common: …", NOT the draft's "shares: …".
	//   - Read state copy is EXACTLY "Readable once your hoard reaches X" (X = THEIR total,
	//     read_state.need_bytes via fmtLargestUnit — never "N more TB").
	//   - Cold-start Interests chips are RETRIEVED FROM TOPICS (topicDiscoverPaint over
	//     TOPIC_ROOTS), never a hardcoded list; on a failed/empty discovery the non-chip actions
	//     (the explainer + "Add a collection") render without chips.
	import { onMount } from 'svelte';
	import {
		similarPeople,
		topicDiscoverPaint,
		getProfile,
		saveProfile,
		hasPublishedProfile,
		publishProfile,
		follow,
	} from '$lib/api.js';
	import type { DiscoveredTopic, PeopleResult, Profile, SimilarPerson } from '$lib/types.js';
	import { contacts, profile as profileStore, toast } from '$lib/stores.js';
	import { TOPIC_ROOTS } from '$lib/topics-view.js';
	import { fmtLargestUnit } from '$lib/browse-view.js';
	import EmptyState from './EmptyState.svelte';
	import Avatar from './Avatar.svelte';

	const CHIP_CAP = 24;

	let result: PeopleResult | null = null;
	let loading = true;
	let loadFailed = false;
	let chips: string[] = [];
	let openPop: string | null = null; // npub of the card whose profile popover is open
	let added: string[] = []; // npubs added this session (the store refetch is the page's job)
	let adding: string[] = []; // npubs with an in-flight `follow`
	let savingChip = false;

	$: titles = (result?.people ?? []).filter((p) => p.reason === 'titles_in_common');
	$: interests = (result?.people ?? []).filter((p) => p.reason === 'interests_only');

	function nameFor(p: SimilarPerson): string {
		return p.display_name ?? p.npub.slice(0, 12) + '…';
	}

	function reasonLine(p: SimilarPerson): string {
		if (p.reason === 'titles_in_common') {
			return `${p.shared_titles} title${p.shared_titles === 1 ? '' : 's'} in common`;
		}
		return p.shared_interests.length ? `In common: ${p.shared_interests.join(', ')}` : 'Interests in common';
	}

	/** The cold-start chip wall: the distinct tags AND names of one `topicDiscoverPaint` over all
	 *  TOPIC_ROOTS, most common first (ties alphabetical), capped. Never a hardcoded list — a
	 *  failed or empty discovery simply yields no chips. */
	function chipWall(topics: DiscoveredTopic[]): string[] {
		const seen = new Map<string, { label: string; n: number }>();
		const bump = (raw: string | undefined) => {
			const label = (raw ?? '').trim();
			if (!label) return;
			const key = label.toLowerCase();
			const e = seen.get(key);
			if (e) e.n += 1;
			else seen.set(key, { label, n: 1 });
		};
		for (const t of topics) {
			for (const tag of t.tags ?? []) bump(tag);
			bump(t.name);
		}
		return [...seen.values()]
			.sort((a, b) => b.n - a.n || a.label.localeCompare(b.label))
			.slice(0, CHIP_CAP)
			.map((e) => e.label);
	}

	async function load(): Promise<void> {
		loading = true;
		loadFailed = false;
		try {
			result = await similarPeople();
			if (result.cold_start) void loadChips();
			else chips = [];
		} catch {
			// QURATOR-93: a FAILED load must never read as "no one like you".
			result = null;
			loadFailed = true;
		} finally {
			loading = false;
		}
	}

	async function loadChips(): Promise<void> {
		chips = [];
		try {
			chips = chipWall(await topicDiscoverPaint([...TOPIC_ROOTS]));
		} catch {
			chips = []; // discovery failed — the actions render WITHOUT chips, never a fallback list
		}
	}

	/** Add a chip term to my profile Interests (append if absent), then save, republish an
	 *  already-public teaser, and re-run the ranking (profile-picture.ts's save+republish pattern). */
	async function addChip(term: string): Promise<void> {
		if (savingChip) return;
		savingChip = true;
		try {
			const base: Profile =
				(await getProfile()) ?? {
					display_name: '',
					tags: [],
					languages: [],
					social_links: [],
					willing_to: [],
					content_types: [],
					updated: new Date().toISOString(),
				};
			const tags = base.tags.includes(term) ? base.tags : [...base.tags, term];
			const next: Profile = { ...base, tags };
			await saveProfile(next);
			profileStore.set(next);
			if (await hasPublishedProfile()) await publishProfile();
			await load();
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			savingChip = false;
		}
	}

	async function addPerson(npub: string): Promise<void> {
		if (adding.includes(npub)) return;
		adding = [...adding, npub];
		try {
			await follow(npub);
			added = [...added, npub];
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			adding = adding.filter((n) => n !== npub);
		}
	}

	const popIdFor = (npub: string) => `ply-pop-${npub}`;

	onMount(() => {
		void load();
	});
</script>

<div class="ply">
	<header class="ply-head">
		<div class="ply-title">People like you</div>
		<div class="ply-sub">Closest hoards to yours — ranked by the titles and Interests you have in common.</div>
	</header>

	{#if loading}
		<div class="ply-line" aria-live="polite">Loading…</div>
	{:else if loadFailed}
		<EmptyState error centered message="People like you could not be loaded." onretry={load} />
	{:else if result?.cold_start}
		<div class="ply-cold">
			<div class="ply-line">
				People like you compares your Interests and your collection titles with other people's, and
				surfaces the closest hoards. There's nothing to compare yet — pick your Interests, or add a
				collection, and matches will show up here.
			</div>
			{#if chips.length}
				<div class="ply-chipwall" aria-label="Pick your Interests">
					{#each chips as term (term)}
						<button type="button" class="ply-chip" disabled={savingChip} onclick={() => addChip(term)}>{term}</button>
					{/each}
				</div>
			{/if}
			<a class="ply-add-collection" href="/">Add a collection</a>
		</div>
	{:else if !(result?.people ?? []).length}
		<EmptyState message="Nothing to compare yet — pick your Interests or add a collection and matches will show up here." />
	{:else}
		{#if titles.length}
			<div class="ply-group-label">Most titles in common</div>
			<div class="ply-grid" style="grid-template-columns: repeat(3, 1fr);">
				{#each titles as p (p.npub)}
					{@render card(p)}
				{/each}
			</div>
		{/if}
		{#if interests.length}
			<div class="group-label-spacer"></div>
			<div class="ply-group-label">Close on Interests</div>
			<div class="ply-grid" style="grid-template-columns: repeat(3, 1fr);">
				{#each interests as p (p.npub)}
					{@render card(p)}
				{/each}
			</div>
		{/if}
	{/if}
</div>

{#snippet card(p: SimilarPerson)}
	{@const c = $contacts.find((x) => x.npub === p.npub)}
	{@const prof = c?.profile}
	<div
		class="ply-card"
		tabindex="0"
		aria-describedby={openPop === p.npub ? popIdFor(p.npub) : undefined}
		onmouseenter={() => (openPop = p.npub)}
		onmouseleave={() => {
			if (openPop === p.npub) openPop = null;
		}}
		onfocus={() => (openPop = p.npub)}
		onblur={() => {
			if (openPop === p.npub) openPop = null;
		}}
		onkeydown={(e) => {
			if (e.key === 'Escape') openPop = null;
		}}
	>
		<div class="ply-top">
			<Avatar letter={(nameFor(p).charAt(0) || '?').toUpperCase()} size={28} picture={prof?.picture} />
			<span class="ply-name">{nameFor(p)}</span>
		</div>
		{#if p.fingerprint}
			<div class="ply-fp">
				<span class="ply-fp-dot" style="background:{p.fingerprint.colorHex}"></span>
				<span class="ply-fp-words">{p.fingerprint.words.join(' ')}</span>
			</div>
		{/if}
		<div class="ply-reason">{reasonLine(p)}</div>
		<div class="ply-actions">
			{#if p.read_state?.kind === 'readable'}
				<a class="btn-default btn-xs" href={'/browse?peer=' + p.npub}>Browse</a>
			{:else if p.read_state?.kind === 'locked'}
				<span class="ply-locked">Readable once your hoard reaches {fmtLargestUnit(p.read_state.need_bytes)}</span>
			{:else if p.read_state?.kind === 'asked'}
				<span class="ply-locked">Access request sent</span>
			{/if}
			<!-- Reads $contacts directly (not a get() snapshot) so a roster refresh hides Add. -->
			{#if !$contacts.some((x) => x.npub === p.npub) && !added.includes(p.npub)}
				<button type="button" class="btn-ghost btn-xs" disabled={adding.includes(p.npub)} onclick={() => addPerson(p.npub)}>+ Add</button>
			{/if}
		</div>

		{#if openPop === p.npub}
			<div class="ply-pop" role="tooltip" id={popIdFor(p.npub)}>
				{#if prof}
					{#if prof.bio}<div class="ply-pop-row"><span class="ply-pop-k">Bio</span><span>{prof.bio}</span></div>{/if}
					{#if prof.tags?.length}<div class="ply-pop-row"><span class="ply-pop-k">Interests</span><span>{prof.tags.join(', ')}</span></div>{/if}
					{#if prof.content_types?.length}<div class="ply-pop-row"><span class="ply-pop-k">Content</span><span>{prof.content_types.join(', ')}</span></div>{/if}
					{#if prof.contact_hint}<div class="ply-pop-row"><span class="ply-pop-k">Contact hint</span><span>{prof.contact_hint}</span></div>{/if}
					{#if prof.teaser_collections?.length}
						<div class="ply-pop-k ply-pop-head">Public collections</div>
						{#each prof.teaser_collections.slice(0, 5) as tc}
							<div class="ply-pop-col"><span class="ply-pop-colname">{tc.name}</span><span class="ply-pop-colsize">{fmtLargestUnit(tc.bytes)}</span></div>
						{/each}
					{/if}
				{:else}
					<div class="ply-pop-row"><span class="ply-pop-k">In common</span><span>{reasonLine(p)}</span></div>
			{/if}
			</div>
		{/if}
	</div>
{/snippet}

<style>
	.ply {
		display: flex;
		flex-direction: column;
		gap: 10px;
		min-width: 0;
	}
	.ply-head { display: flex; flex-direction: column; gap: 2px; }
	.ply-title { font-size: 13px; font-weight: 600; color: var(--fg); }
	.ply-sub { font-size: 12px; color: var(--fg-dim); line-height: 1.45; }

	.ply-line { font-size: 12.5px; color: var(--fg-dim); line-height: 1.5; }

	.ply-group-label { font-size: 11px; font-weight: 600; letter-spacing: 0.04em; text-transform: uppercase; color: var(--fg-dim); }
	.group-label-spacer { height: 6px; }

	.ply-grid {
		display: grid;
		gap: 10px;
		align-items: stretch;
	}

	.ply-card {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 10px;
		border-radius: 8px;
		background: var(--bg-elev1);
		border: 1px solid oklch(1 0 0 / 0.06);
		cursor: default;
		outline: none;
	}
	.ply-card:focus-visible { border-color: var(--accent); }

	.ply-top { display: flex; align-items: center; gap: 8px; min-width: 0; }
	.ply-name { font-size: 12.5px; font-weight: 600; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

	.ply-fp { display: flex; align-items: center; gap: 6px; font-size: 11.5px; color: var(--fg-dim); }
	.ply-fp-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }
	.ply-fp-words { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

	.ply-reason { font-size: 12px; color: var(--fg); }

	.ply-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: auto; }
	.ply-locked { font-size: 11.5px; color: var(--fg-dim); line-height: 1.4; }

	.ply-pop {
		position: absolute;
		top: calc(100% + 6px);
		left: 0;
		z-index: 30;
		width: 260px;
		padding: 10px;
		border-radius: 8px;
		background: var(--bg-elev2);
		border: 1px solid oklch(1 0 0 / 0.12);
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.35);
		display: flex;
		flex-direction: column;
		gap: 6px;
		font-size: 11.5px;
		color: var(--fg);
	}
	.ply-pop-row { display: flex; flex-direction: column; gap: 1px; }
	.ply-pop-k { color: var(--fg-dim); font-size: 10.5px; text-transform: uppercase; letter-spacing: 0.04em; }
	.ply-pop-head { margin-top: 2px; }
	.ply-pop-col { display: flex; justify-content: space-between; gap: 8px; }
	.ply-pop-colname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.ply-pop-colsize { color: var(--fg-dim); flex-shrink: 0; }

	.ply-cold { display: flex; flex-direction: column; gap: 10px; }
	.ply-chipwall { display: flex; flex-wrap: wrap; gap: 6px; }
	.ply-chip {
		border: 1px solid oklch(1 0 0 / 0.12);
		background: var(--bg-elev1);
		color: var(--fg);
		font-size: 11.5px;
		padding: 3px 10px;
		border-radius: 999px;
		cursor: pointer;
	}
	.ply-chip:hover { background: var(--bg-elev2); }
	.ply-chip:disabled { opacity: 0.6; cursor: default; }
	.ply-add-collection { font-size: 12px; color: var(--accent); text-decoration: none; }
	.ply-add-collection:hover { text-decoration: underline; }
</style>
