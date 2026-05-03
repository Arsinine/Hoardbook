<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { contacts, identity, inboxMessages, sentMessages, unreadCount, toast } from '$lib/stores.js';
	import { getMessages, sendMessage, getChannelMessages, postChannelMessage } from '$lib/api.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import type { CachedPeer, ReceivedChannelMessage, ReceivedMessage } from '$lib/types.js';

	let loading = false;
	let sending = false;
	let selectedPeer: CachedPeer | null = null;
	let showGeneral = false;
	let draft = '';
	let threadEl: HTMLElement;
	let generalThreadEl: HTMLElement;

	// Stable set of message keys we've already counted for badge purposes.
	// Format: `${from}|${sent_at}` — prevents double-counting on relay inconsistencies.
	let seenMessageKeys = new Set<string>();

	// Per-peer "seen" snapshot: hb_id → inbox count at last view.
	let seenCounts: Record<string, number> = {};

	// General channel state
	let channelMessages: ReceivedChannelMessage[] = [];
	let channelDraft = '';
	let sendingChannel = false;
	let channelLoading = false;

	async function loadChannel() {
		channelLoading = true;
		try {
			channelMessages = await getChannelMessages('general');
			await tick();
			if (generalThreadEl) generalThreadEl.scrollTop = generalThreadEl.scrollHeight;
		} catch { /* relay may be unreachable */ }
		finally { channelLoading = false; }
	}

	async function selectGeneral() {
		showGeneral = true;
		selectedPeer = null;
		await loadChannel();
	}

	async function handleChannelSend() {
		if (!channelDraft.trim() || sendingChannel) return;
		sendingChannel = true;
		const content = channelDraft.trim();
		channelDraft = '';
		try {
			const sent = await postChannelMessage('general', content);
			channelMessages = [...channelMessages, sent];
			await tick();
			if (generalThreadEl) generalThreadEl.scrollTop = generalThreadEl.scrollHeight;
		} catch (e) {
			toast(String(e), 'error');
			channelDraft = content;
		} finally {
			sendingChannel = false;
		}
	}

	function handleChannelKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleChannelSend(); }
	}

	$: myId = $identity?.hb_id ?? '';

	// Merge inbox senders who aren't contacts into a unified conversation list.
	// This lets recipients see DMs from people who haven't followed them back.
	$: inboxSenderIds = [...new Set($inboxMessages.map(m => m.from))];
	$: inboxOnlyPeers = inboxSenderIds
		.filter(id => id !== myId && !$contacts.some(c => c.hb_id === id))
		.map(id => ({ hb_id: id, profile: undefined, collections: [], online: false, node_addr: undefined, last_fetched: '', last_seen_at: undefined, local_tags: [] } satisfies CachedPeer));

	$: allConversationPeers = [...$contacts, ...inboxOnlyPeers];

	$: conversation = selectedPeer
		? [
				...$inboxMessages.filter((m) => m.from === selectedPeer!.hb_id),
				...$sentMessages.filter((m) => m.to === selectedPeer!.hb_id)
			].sort((a, b) => a.sent_at.localeCompare(b.sent_at))
		: [];

	// Resolve display name for a sender hb_id (for general chat).
	function senderName(hb_id: string): string {
		if (hb_id === myId) return 'You';
		const contact = $contacts.find(c => c.hb_id === hb_id);
		if (contact?.profile?.display_name) return contact.profile.display_name;
		return shortId(hb_id);
	}

	onMount(() => {
		// Clear unread badge when entering the chat page.
		unreadCount.set(0);
		refreshInbox(); // fire-and-forget; handles its own loading state
		// Message polling is handled by +layout.svelte so the nav badge works from any page.
	});

	async function refreshInbox() {
		if (!$identity) return;
		loading = true;
		try {
			const msgs = await getMessages();
			// Seed seen keys so layout poll doesn't double-badge already-fetched messages.
			for (const m of msgs) seenMessageKeys.add(`${m.from}|${m.sent_at}`);
			inboxMessages.set(msgs);
			unreadCount.set(0);
			// Seed per-peer seen counts from current inbox so remounting shows no false unread.
			for (const m of msgs) {
				seenCounts[m.from] = msgs.filter(x => x.from === m.from).length;
			}
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			loading = false;
		}
	}

	async function selectPeer(peer: CachedPeer) {
		selectedPeer = peer;
		showGeneral = false;
		seenCounts[peer.hb_id] = $inboxMessages.filter((m) => m.from === peer.hb_id).length;
		await tick();
		scrollToBottom();
	}

	async function handleSend() {
		if (!selectedPeer || !draft.trim() || sending) return;
		sending = true;
		const content = draft.trim();
		draft = '';
		try {
			const sent = await sendMessage(selectedPeer.hb_id, content);
			// Track sent message so poll doesn't re-badge it.
			seenMessageKeys.add(`${sent.from}|${sent.sent_at}`);
			sentMessages.update((prev) => [...prev, sent]);
			await tick();
			scrollToBottom();
		} catch (e) {
			toast(String(e), 'error');
			draft = content;
		} finally {
			sending = false;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			handleSend();
		}
	}

	function scrollToBottom() {
		if (threadEl) threadEl.scrollTop = threadEl.scrollHeight;
	}

	function shortId(hb_id: string) {
		return hb_id.length > 16 ? hb_id.slice(0, 8) + '…' + hb_id.slice(-4) : hb_id;
	}

	function formatTime(iso: string) {
		return new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	function formatDate(iso: string) {
		return new Date(iso).toLocaleDateString([], { month: 'short', day: 'numeric' });
	}

	$: unreadCounts = Object.fromEntries(
		allConversationPeers.map((c) => {
			const total = $inboxMessages.filter((m) => m.from === c.hb_id).length;
			const seen = seenCounts[c.hb_id] ?? 0;
			return [c.hb_id, Math.max(0, total - seen)];
		})
	);

	function viewProfile(peer: CachedPeer) {
		goto('/contacts');
	}

	// Show a privacy notice if the selected peer is not in contacts (may have DMs restricted).
	$: selectedIsContact = selectedPeer ? $contacts.some(c => c.hb_id === selectedPeer!.hb_id) : false;
</script>

{#if !$identity}
	<div class="no-identity">
		<p>No identity yet.</p>
		<a href="/settings" class="btn-primary">Go to Settings →</a>
	</div>
{:else}
	<div class="chat-frame">
		<!-- Conversation list -->
		<div class="convo-sidebar">
			<div class="convo-header">
				<span class="convo-title">Conversations</span>
				<button class="icon-btn" on:click={refreshInbox} disabled={loading} title="Refresh inbox">
					{@html icons.refresh}
				</button>
			</div>
			<div class="convo-search">
				<div class="search-wrap">
					<span class="search-icon-sm">{@html icons.search}</span>
					<input class="search-bare" type="text" placeholder="Search…" />
				</div>
			</div>
			<div class="convo-list">
				<!-- General channel — always pinned at top -->
				<button class="convo-item convo-general" class:convo-active={showGeneral} on:click={selectGeneral}>
					<div class="general-icon">#</div>
					<div class="convo-info">
						<div class="convo-row">
							<span class="convo-name" class:convo-name-active={showGeneral}>General</span>
						</div>
						<div class="convo-preview-row">
							<span class="convo-preview-text">Community channel</span>
						</div>
					</div>
				</button>
				<div class="convo-divider">Direct Messages</div>
				{#if allConversationPeers.length === 0}
					<div class="convo-empty">Add contacts via Contacts to start chatting.</div>
				{:else}
					{#each allConversationPeers as peer}
						{@const name = peer.profile?.display_name ?? shortId(peer.hb_id)}
						{@const initial = name[0]?.toUpperCase() ?? '?'}
						{@const hue = avatarHue(initial)}
						{@const unread = unreadCounts[peer.hb_id] ?? 0}
						{@const active = selectedPeer?.hb_id === peer.hb_id}
						{@const isContact = $contacts.some(c => c.hb_id === peer.hb_id)}
						<button class="convo-item" class:convo-active={active} on:click={() => selectPeer(peer)}>
							<Avatar letter={initial} size={34} {hue} />
							<div class="convo-info">
								<div class="convo-row">
									<span class="convo-name" class:convo-name-active={active}>{name}</span>
									{#if !isContact}<span class="convo-req-dot" title="Message request" />{/if}
								</div>
								<div class="convo-preview-row">
									{#if unread > 0}
										<span class="unread-badge">{unread}</span>
									{/if}
								</div>
							</div>
						</button>
					{/each}
				{/if}
			</div>
		</div>

		<!-- Conversation pane -->
		<div class="convo-pane">
			{#if showGeneral}
				<div class="pane-header">
					<div class="general-icon general-icon-lg">#</div>
					<div class="pane-peer-info">
						<div class="pane-peer-name">General</div>
						<span class="mono">Community channel — public, unencrypted</span>
					</div>
					<button class="icon-btn" on:click={loadChannel} disabled={channelLoading} title="Refresh">
						{@html icons.refresh}
					</button>
				</div>
				<div class="privacy-banner">
					<span class="privacy-icon">{@html icons.shield}</span>
					<span>All messages in this channel are public and visible to anyone on the relay.</span>
				</div>
				<div class="thread" bind:this={generalThreadEl}>
					{#if channelLoading}
						<p class="thread-empty">Loading…</p>
					{:else if channelMessages.length === 0}
						<p class="thread-empty">No messages yet. Be the first to say hello!</p>
					{:else}
						{#each channelMessages as msg, i}
							{@const isMe = msg.from === myId}
							{@const prevMsg = i > 0 ? channelMessages[i - 1] : null}
							{@const showDate = !prevMsg || formatDate(msg.sent_at) !== formatDate(prevMsg.sent_at)}
							{#if showDate}
								<div class="day-marker">
									<div class="day-line" />
									<span class="day-label">{formatDate(msg.sent_at)}</span>
									<div class="day-line" />
								</div>
							{/if}
							<div class="channel-msg" class:channel-msg-me={isMe}>
								<span class="channel-sender">{senderName(msg.from)}</span>
								<span class="channel-time">{formatTime(msg.sent_at)}</span>
								<p class="channel-text">{msg.content}</p>
							</div>
						{/each}
					{/if}
				</div>
				{#if $identity}
					<div class="composer">
						<div class="compose-box">
							<textarea
								class="compose-input"
								placeholder="Post to #general…"
								bind:value={channelDraft}
								on:keydown={handleChannelKeydown}
								disabled={sendingChannel}
								rows="2"
							></textarea>
							<div class="compose-footer">
								<span class="compose-hint">Public · no encryption</span>
								<button
									class="btn-primary btn-sm btn-icon"
									on:click={handleChannelSend}
									disabled={!channelDraft.trim() || sendingChannel}
								>
									{sendingChannel ? '…' : 'Post'}
									<span>{@html icons.send}</span>
								</button>
							</div>
						</div>
					</div>
				{/if}
			{:else if !selectedPeer}
				<div class="convo-empty-state">
					<p>Select a contact to view the conversation.</p>
					<p class="privacy-note">
						{@html icons.shield} Messages are stored unencrypted on relay servers and are publicly readable.
					</p>
				</div>
			{:else}
				<!-- Header -->
				<div class="pane-header">
					<Avatar
						letter={(selectedPeer.profile?.display_name ?? selectedPeer.hb_id)[0].toUpperCase()}
						size={36}
						hue={avatarHue((selectedPeer.profile?.display_name ?? selectedPeer.hb_id)[0])}
					/>
					<div class="pane-peer-info">
						<div class="pane-peer-row">
							<span class="pane-peer-name">{selectedPeer.profile?.display_name ?? shortId(selectedPeer.hb_id)}</span>
							{#if selectedPeer.online}
								<span class="pill pill-online"><span class="pill-dot" /> Online</span>
							{:else}
								<span class="pill pill-offline">Offline</span>
							{/if}
						</div>
						<span class="mono">{shortId(selectedPeer.hb_id)}</span>
					</div>
					<button class="btn-ghost btn-sm" on:click={() => { if (selectedPeer) viewProfile(selectedPeer); }}>View profile</button>
				</div>

				<!-- Privacy banner -->
				<div class="privacy-banner">
					<span class="privacy-icon">{@html icons.shield}</span>
					<span>Messages are unencrypted and publicly readable on relays. Don't share secrets.</span>
				</div>

				<!-- Offline notice -->
				{#if !selectedPeer.online}
					<div class="offline-banner">
						<span class="offline-dot" />
						<span>{selectedPeer.profile?.display_name ?? shortId(selectedPeer.hb_id)} is offline — your message will be delivered when they come online.</span>
					</div>
				{/if}

				<!-- Notice for message requests (sender not in recipient's contacts) -->
				{#if !selectedIsContact}
					<div class="request-banner">
						<span>This person may not have followed you back — their privacy settings may filter your messages.</span>
					</div>
				{/if}

				<!-- Thread -->
				<div class="thread" bind:this={threadEl}>
					{#if conversation.length === 0}
						<p class="thread-empty">No messages yet. Say hello!</p>
					{:else}
						{#each conversation as msg, i}
							{@const isMe = msg.from === myId}
							{@const prevMsg = i > 0 ? conversation[i - 1] : null}
							{@const showDate = !prevMsg || formatDate(msg.sent_at) !== formatDate(prevMsg.sent_at)}
							{#if showDate}
								<div class="day-marker">
									<div class="day-line" />
									<span class="day-label">{formatDate(msg.sent_at)}</span>
									<div class="day-line" />
								</div>
							{/if}
							<div class="bubble-wrap" class:bubble-me={isMe}>
								<div class="bubble" class:bubble-sent={isMe} class:bubble-recv={!isMe}>
									<p class="bubble-text">{msg.content}</p>
									<span class="bubble-time">{formatTime(msg.sent_at)}</span>
								</div>
							</div>
						{/each}
					{/if}
				</div>

				<!-- Compose -->
				<div class="composer">
					<div class="compose-box">
						<textarea
							class="compose-input"
							placeholder="Type a message…"
							bind:value={draft}
							on:keydown={handleKeydown}
							disabled={sending}
							rows="2"
						></textarea>
						<div class="compose-footer">
							<button
								class="btn-primary btn-send"
								on:click={handleSend}
								disabled={!draft.trim() || sending}
							>
								{sending ? '…' : 'Send'} <span>{@html icons.send}</span>
							</button>
						</div>
					</div>
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.no-identity {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		gap: 12px;
		color: var(--fg-muted);
	}

	.chat-frame { display: flex; height: 100%; overflow: hidden; }

	/* Conversation list sidebar */
	.convo-sidebar {
		width: 240px;
		flex-shrink: 0;
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		background: var(--bg);
	}

	.convo-header {
		padding: 16px 16px 10px;
		border-bottom: 1px solid var(--border);
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.convo-title { font-size: 14px; font-weight: 600; }

	.icon-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-muted);
		display: flex;
		padding: 2px;
	}
	.icon-btn:disabled { opacity: 0.5; }

	.convo-search { padding: 10px 12px; border-bottom: 1px solid var(--divider); }

	.search-wrap {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 10px;
		height: 30px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}

	.search-icon-sm { color: var(--fg-dim); display: flex; }

	.search-bare {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 12.5px;
		color: var(--fg);
	}
	.search-bare::placeholder { color: var(--fg-dim); }

	.convo-list { flex: 1; overflow-y: auto; padding: 6px 8px; }

	.convo-empty { padding: 12px; font-size: 12px; color: var(--fg-dim); }

	.convo-divider {
		padding: 10px 12px 4px;
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 1px;
		color: var(--fg-dim);
	}

	.convo-general { margin-bottom: 2px; }

	.general-icon {
		width: 34px; height: 34px;
		border-radius: 9px;
		background: var(--accent-soft);
		border: 1px solid color-mix(in oklch, var(--accent) 25%, transparent);
		display: flex; align-items: center; justify-content: center;
		font-size: 16px; font-weight: 700;
		color: var(--accent);
		flex-shrink: 0;
	}

	.general-icon-lg {
		width: 36px; height: 36px;
		border-radius: 10px;
		font-size: 18px;
	}

	.convo-preview-text { font-size: 11px; color: var(--fg-dim); }

	/* Channel messages */
	.channel-msg {
		padding: 6px 0;
		border-bottom: 1px solid var(--divider);
	}
	.channel-msg:last-child { border-bottom: none; }
	.channel-msg-me .channel-sender { color: var(--accent); }

	.channel-sender {
		font-size: 11.5px;
		font-weight: 600;
		color: var(--fg-muted);
		margin-right: 8px;
	}

	.channel-time {
		font-size: 10.5px;
		color: var(--fg-dim);
	}

	.channel-text {
		font-size: 13px;
		line-height: 1.5;
		white-space: pre-wrap;
		word-break: break-word;
		margin: 3px 0 0;
		color: var(--fg);
	}

	.compose-hint {
		font-size: 11px;
		color: var(--fg-dim);
	}

	.convo-item {
		width: 100%;
		display: flex;
		gap: 10px;
		align-items: center;
		padding: 10px;
		background: transparent;
		border: none;
		border-radius: 7px;
		cursor: pointer;
		color: inherit;
		font-family: inherit;
		margin-bottom: 2px;
		text-align: left;
	}
	.convo-item:hover { background: var(--bg-elev1); }
	.convo-active { background: var(--bg-elev2); }

	.convo-info { flex: 1; min-width: 0; }

	.convo-row { display: flex; justify-content: space-between; align-items: center; gap: 4px; }

	.convo-name { font-size: 13px; font-weight: 500; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
	.convo-name-active { font-weight: 600; }

	.convo-req-dot {
		width: 6px; height: 6px; border-radius: 50%;
		background: oklch(0.75 0.16 60); flex-shrink: 0;
	}

	.convo-preview-row { display: flex; align-items: center; margin-top: 2px; gap: 4px; }

	.unread-badge {
		font-size: 10px;
		padding: 1px 6px;
		border-radius: 999px;
		background: var(--accent);
		color: var(--accent-text);
		font-weight: 700;
		min-width: 16px;
		text-align: center;
		font-feature-settings: 'tnum';
	}

	/* Conversation pane */
	.convo-pane {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: var(--bg-elev1);
	}

	.convo-empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 12px;
		padding: 32px;
		color: var(--fg-muted);
	}

	.privacy-note { font-size: 12px; color: var(--fg-dim); text-align: center; max-width: 320px; }

	.pane-header {
		padding: 12px 18px;
		border-bottom: 1px solid var(--border);
		display: flex;
		gap: 12px;
		align-items: center;
		background: var(--bg);
	}

	.pane-peer-info { flex: 1; min-width: 0; }

	.pane-peer-row { display: flex; align-items: center; gap: 8px; margin-bottom: 2px; }

	.pane-peer-name { font-weight: 600; font-size: 14px; }

	.mono { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	.privacy-banner {
		padding: 8px 18px;
		background: var(--accent-soft);
		border-bottom: 1px solid var(--border);
		font-size: 11.5px;
		color: var(--fg);
		display: flex;
		gap: 8px;
		align-items: center;
	}

	.privacy-icon { color: var(--accent); display: flex; }

	.offline-banner {
		padding: 7px 18px;
		background: color-mix(in oklch, var(--fg-dim) 8%, transparent);
		border-bottom: 1px solid var(--border);
		font-size: 11.5px;
		color: var(--fg-muted);
		display: flex;
		gap: 8px;
		align-items: center;
	}
	.offline-dot {
		width: 7px; height: 7px; border-radius: 50%;
		background: var(--fg-dim); flex-shrink: 0;
	}

	.request-banner {
		padding: 6px 18px;
		background: oklch(0.22 0.06 60 / 0.6);
		border-bottom: 1px solid oklch(0.45 0.12 60 / 0.3);
		font-size: 11.5px;
		color: oklch(0.82 0.12 60);
	}

	.thread {
		flex: 1;
		padding: 20px 24px;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.thread-empty { color: var(--fg-dim); font-size: 13px; text-align: center; padding-top: 32px; }

	.day-marker { display: flex; align-items: center; gap: 10px; margin: 12px 0 8px; }

	.day-line { flex: 1; height: 1px; background: var(--divider); }

	.day-label { font-size: 10.5px; color: var(--fg-dim); text-transform: uppercase; letter-spacing: 1px; white-space: nowrap; }

	.bubble-wrap { display: flex; margin-bottom: 4px; }
	.bubble-me { justify-content: flex-end; }

	.bubble {
		max-width: 70%;
		padding: 8px 12px;
		border-radius: 14px;
	}

	.bubble-sent {
		background: var(--accent);
		color: var(--accent-text);
		border-radius: 14px 14px 4px 14px;
	}

	.bubble-recv {
		background: var(--bg-elev2);
		color: var(--fg);
		border: 1px solid var(--border);
		border-radius: 14px 14px 14px 4px;
	}

	.bubble-text { font-size: 13px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; margin: 0; }

	.bubble-time { font-size: 10px; color: inherit; opacity: 0.6; display: block; text-align: right; margin-top: 3px; }

	.composer {
		padding: 14px;
		border-top: 1px solid var(--border);
		background: var(--bg);
	}

	.compose-box {
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 9px;
		padding: 10px 12px;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.compose-input {
		width: 100%;
		background: transparent;
		border: none;
		outline: none;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		resize: none;
		min-height: 36px;
	}
	.compose-input::placeholder { color: var(--fg-dim); }

	.compose-footer { display: flex; justify-content: flex-end; align-items: center; }

	.btn-send {
		display: inline-flex; align-items: center; justify-content: center; gap: 5px;
		padding: 6px 14px;
		font-family: var(--font-ui); font-size: 12px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
		min-width: 68px;
	}
	.btn-send:disabled { opacity: 0.5; cursor: not-allowed; }

	/* Pills */
	.pill {
		display: inline-flex; align-items: center; gap: 5px;
		font-size: 10.5px; font-weight: 500;
		padding: 2px 8px; border-radius: 999px;
	}
	.pill-dot { width: 5px; height: 5px; border-radius: 50%; }
	.pill-online {
		color: var(--online);
		background: color-mix(in oklch, var(--online) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--online) 20%, transparent);
	}
	.pill-online .pill-dot { background: var(--online); }
	.pill-offline {
		color: var(--fg-muted);
		background: color-mix(in oklch, var(--fg-muted) 12%, transparent);
		border: 1px solid color-mix(in oklch, var(--fg-muted) 20%, transparent);
	}

	/* Buttons */
	.btn-primary {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 600;
		color: var(--accent-text); background: var(--accent);
		border: 1px solid var(--accent); border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}
	.btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
	.btn-ghost {
		display: inline-flex; align-items: center; justify-content: center; gap: 6px;
		padding: 8px 14px; font-family: var(--font-ui); font-size: 13px; font-weight: 500;
		color: var(--fg-muted); background: transparent;
		border: 1px solid transparent; border-radius: 7px;
		cursor: pointer; white-space: nowrap; user-select: none; line-height: 1;
	}
	.btn-sm { padding: 5px 11px; font-size: 12px; }
	.btn-icon { gap: 5px; }
</style>
