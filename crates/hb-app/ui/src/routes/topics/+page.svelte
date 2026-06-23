<script lang="ts">
	import { onMount } from 'svelte';
	import { toast } from '$lib/stores.js';
	import {
		topicList,
		topicCreate,
		topicDiscover,
		topicJoinPublic,
		topicRedeemInvite,
		topicLeave,
		topicInvite,
		topicRoster,
		topicChannel,
		topicPost,
	} from '$lib/api.js';
	import type { TopicView, DiscoveredTopic, ChannelPost } from '$lib/types.js';
	import { memberCountLabel } from '$lib/topics-view.js';
	import TopicJoinConsent from '$lib/components/TopicJoinConsent.svelte';

	let mine: TopicView[] = [];
	let discovered: DiscoveredTopic[] = [];
	let busy = false;

	// Create form
	let newName = '';
	let newDesc = '';
	let newTags = '';
	let newPrivate = false;

	// Discover
	let searchTags = '';

	// The consent gate: which Topic (public name + private flag) is pending a join.
	let pendingJoin: { name: string; isPrivate: boolean } | null = null;

	// Open Topic (roster + channel)
	let openTopic: TopicView | null = null;
	let roster: string[] = [];
	let posts: ChannelPost[] = [];
	let draft = '';
	let inviteNpub = '';

	function splitTags(s: string): string[] {
		return s.split(',').map((t) => t.trim()).filter(Boolean);
	}

	async function loadMine() {
		try {
			mine = await topicList();
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	onMount(loadMine);

	async function create() {
		if (!newName.trim()) return;
		busy = true;
		try {
			await topicCreate(newName.trim(), newDesc.trim(), splitTags(newTags), newPrivate);
			newName = newDesc = newTags = '';
			newPrivate = false;
			await loadMine();
			toast('Topic created', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function discover() {
		const tags = splitTags(searchTags);
		if (tags.length === 0) {
			toast('Enter at least one tag to discover Topics', 'error');
			return;
		}
		busy = true;
		try {
			discovered = await topicDiscover(tags);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	// Join is always gated behind the consent component (F12) — even for public Topics.
	function askToJoin(name: string, isPrivate: boolean) {
		pendingJoin = { name, isPrivate };
	}

	async function confirmJoin() {
		if (!pendingJoin) return;
		busy = true;
		try {
			await topicJoinPublic(pendingJoin.name);
			pendingJoin = null;
			await loadMine();
			toast('Joined Topic', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function redeemInvite() {
		busy = true;
		try {
			const joined = await topicRedeemInvite();
			if (joined) {
				await loadMine();
				toast(`Joined private Topic “${joined.name}”`, 'success');
			} else {
				toast('No pending invite found', 'success');
			}
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function leave(t: TopicView) {
		busy = true;
		try {
			await topicLeave(t.topic_id);
			if (openTopic?.topic_id === t.topic_id) openTopic = null;
			await loadMine();
			toast('Left Topic', 'success');
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			busy = false;
		}
	}

	async function open(t: TopicView) {
		openTopic = t;
		roster = [];
		posts = [];
		try {
			roster = await topicRoster(t.topic_id);
			posts = await topicChannel(t.topic_id);
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function send() {
		if (!openTopic || !draft.trim()) return;
		try {
			await topicPost(openTopic.topic_id, draft.trim());
			draft = '';
			posts = await topicChannel(openTopic.topic_id);
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function invite() {
		if (!openTopic || !inviteNpub.trim()) return;
		try {
			await topicInvite(openTopic.topic_id, inviteNpub.trim());
			inviteNpub = '';
			toast('Invite sent', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}
</script>

<div class="page">
	<header><h1>Topics</h1></header>

	<section class="cols">
		<!-- Left: create + discover -->
		<div class="col">
			<div class="card">
				<h2>Create a Topic</h2>
				<input placeholder="name (e.g. 80s-anime)" bind:value={newName} />
				<input placeholder="description" bind:value={newDesc} />
				<input placeholder="tags, comma-separated" bind:value={newTags} />
				<label class="check"><input type="checkbox" bind:checked={newPrivate} /> Private (unlisted)</label>
				<button disabled={busy || !newName.trim()} on:click={create}>Create</button>
			</div>

			<div class="card">
				<h2>Discover public Topics</h2>
				<input placeholder="search tags, comma-separated" bind:value={searchTags} />
				<button disabled={busy} on:click={discover}>Discover</button>
				{#each discovered as d (d.topic_id)}
					<div class="row">
						<div class="grow">
							<div class="name">{d.name}</div>
							<div class="muted">{memberCountLabel(d.member_count_estimate)}</div>
						</div>
						<button on:click={() => askToJoin(d.name, false)}>Join</button>
					</div>
				{/each}
				<button class="link" disabled={busy} on:click={redeemInvite}>Redeem a private invite addressed to me</button>
			</div>
		</div>

		<!-- Right: my topics -->
		<div class="col">
			<div class="card">
				<h2>My Topics</h2>
				{#if mine.length === 0}
					<p class="muted">You haven’t joined any Topics yet.</p>
				{/if}
				{#each mine as t (t.topic_id)}
					<div class="row">
						<div class="grow">
							<div class="name">{t.name} {#if t.private}<span class="tag">private</span>{/if}</div>
							<div class="muted">{t.description}</div>
						</div>
						<button on:click={() => open(t)}>Open</button>
						<button class="ghost" on:click={() => leave(t)}>Leave</button>
					</div>
				{/each}
			</div>

			{#if openTopic}
				<div class="card">
					<h2>{openTopic.name} — roster & 24h channel</h2>
					<div class="muted">Roster ({roster.length})</div>
					<ul class="roster">
						{#each roster as npub (npub)}<li>{npub.slice(0, 12)}…{npub.slice(-4)}</li>{/each}
					</ul>

					<div class="invite">
						<input placeholder="invite an npub…" bind:value={inviteNpub} />
						<button on:click={invite}>Invite</button>
					</div>

					<div class="muted">Channel (wiped after 24h)</div>
					<ul class="channel">
						{#each posts as p (p.author_npub + p.ts)}
							<li><span class="who">{p.author_npub.slice(0, 10)}…</span> {p.body}</li>
						{/each}
					</ul>
					<div class="compose">
						<input placeholder="say something…" bind:value={draft} on:keydown={(e) => e.key === 'Enter' && send()} />
						<button on:click={send}>Post</button>
					</div>
				</div>
			{/if}
		</div>
	</section>
</div>

<!-- F12 consent gate: a join (public or private) fires only after explicit acknowledgment. -->
{#if pendingJoin}
	<!-- svelte-ignore a11y-no-static-element-interactions a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
	<div
		class="modal-backdrop"
		role="dialog"
		aria-modal="true"
		aria-label="Join Topic consent"
		on:click={(e) => { if (e.target === e.currentTarget) pendingJoin = null; }}
	>
		<div class="modal">
			<h2>Join “{pendingJoin.name}”</h2>
			<TopicJoinConsent
				isPrivate={pendingJoin.isPrivate}
				disabled={busy}
				on:join={confirmJoin}
				on:cancel={() => (pendingJoin = null)}
			/>
		</div>
	</div>
{/if}

<style>
	.page { padding: 18px 22px; overflow-y: auto; }
	header h1 { font-size: 18px; font-weight: 700; margin: 0 0 14px; }
	.cols { display: flex; gap: 16px; align-items: flex-start; }
	.col { flex: 1; display: flex; flex-direction: column; gap: 14px; min-width: 0; }
	.card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 14px;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.card h2 { font-size: 13px; font-weight: 600; margin: 0 0 4px; }
	input {
		padding: 6px 9px;
		background: var(--bg-elev2);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 6px;
		font: inherit;
	}
	button {
		padding: 6px 12px;
		border-radius: 6px;
		border: 1px solid var(--border);
		background: var(--bg-elev2);
		color: var(--fg);
		font: inherit;
		cursor: pointer;
	}
	button.ghost, button.link { background: transparent; }
	button.link { border: none; color: var(--accent); text-align: left; padding: 4px 0; }
	.check { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--fg-muted); }
	.row { display: flex; align-items: center; gap: 8px; padding: 6px 0; border-top: 1px solid var(--divider); }
	.row .grow { flex: 1; min-width: 0; }
	.name { font-size: 13px; font-weight: 600; }
	.muted { font-size: 11.5px; color: var(--fg-dim); }
	.tag { font-size: 10px; color: var(--accent); border: 1px solid var(--border); border-radius: 4px; padding: 0 4px; }
	.roster, .channel { list-style: none; margin: 0; padding: 0; font-size: 12px; max-height: 160px; overflow-y: auto; }
	.roster li, .channel li { padding: 3px 0; }
	.channel .who { color: var(--fg-dim); font-family: var(--font-mono); }
	.invite, .compose { display: flex; gap: 6px; }
	.invite input, .compose input { flex: 1; }
	.modal-backdrop {
		position: fixed; inset: 0; z-index: 9998;
		background: oklch(0 0 0 / 0.45);
		display: flex; align-items: center; justify-content: center;
	}
	.modal {
		background: var(--bg-elev1);
		border: 1px solid var(--border-strong);
		border-radius: 12px;
		padding: 18px;
		width: min(440px, 90vw);
	}
	.modal h2 { font-size: 14px; font-weight: 700; margin: 0 0 10px; }
</style>
