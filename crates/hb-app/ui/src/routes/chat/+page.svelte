<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { contacts, identity, inboxMessages, sentMessages, readWatermarks, toast, dmRequests, announceSeen } from '$lib/stores.js';
	import {
		getMessages,
		sendMessage,
		pasteKey,
		topicList,
		topicChannel,
		topicPost,
		getContacts,
		dmRequests as fetchDmRequests,
		dmRequestAccept,
		dmRequestDecline,
		dmBlock,
		groupsGet,
		groupsCreate,
		groupsSetTrusted,
		contactUpdateGroups,
		advanceReadWatermark,
		topicAnnounceMarkSeen,
	} from '$lib/api.js';
	import { icons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import AddContactDialog from '$lib/components/AddContactDialog.svelte';
	import CreateGroupDialog from '$lib/components/CreateGroupDialog.svelte';
	import Modal from '$lib/components/Modal.svelte';
	import { DM_POLL_VISIBLE_MS, CHANNEL_REFRESH_EVERY_TICKS } from '$lib/poll-lifecycle.js';
	import { renderFingerprint } from '$lib/identity-display.js';
	import { contactDisplayName } from '$lib/contact-display.js';
	import { requestBadge, sortRequests, requestPreview, canReply, REQUEST_EXPLAINER, manifestRequestHint } from '$lib/request-inbox.js';
	import { filterConversations, filterTopics, composeRecipientKind, isComposeToSelf } from '$lib/chat-filter.js';
	import { peerPreview, peersWithHistory, relativeTime } from '$lib/chat-preview.js';
	import { sortChannelPostsAscending, resolveTopicParam, interleaveChannel } from '$lib/topics-view.js';
	import { latestFromPeer, unreadByPeer } from '$lib/unread-view.js';
	import type { CachedPeer, ReceivedMessage, TopicView, ChannelPost, AnnouncementView, DmRequestView, Group } from '$lib/types.js';

	let loading = $state(false);
	let sending = $state(false);
	let selectedPeer: CachedPeer | null = $state(null);
	let draft = $state('');
	let threadEl: HTMLElement | undefined = $state();
	let searchQuery = $state('');
	// devtest v0.12.1 #4: a `/chat?peer=<npub>` deep-link (from double-clicking a contact) opens that
	// conversation. Guarded so it fires once per distinct param, and re-evaluates as `$contacts` loads.
	let peerDeepLinked = $state('');

	// ── Topic channels (M11) — a Topic you've joined surfaces here as a persistent channel. The
	//    channel ENTRY lasts as long as your membership (durable, §11), but its posts are 24h-ephemeral
	//    (wiped server-side via NIP-40 + the local filter in `topic_channel`). Posting lives here now,
	//    not on the Topics page (which keeps join/leave/roster/invite).
	let topics: TopicView[] = $state([]);
	let selectedTopic: TopicView | null = $state(null);
	let channelPosts: ChannelPost[] = $state([]);
	let channelAnnouncements: AnnouncementView[] = $state([]); // M13 Part A: read-only member broadcasts
	// devtest #6: announcements are interleaved with posts by timestamp (not pinned above).
	let channelItems = $derived(interleaveChannel(channelPosts, channelAnnouncements));
	let channelDraft = $state('');
	let channelSending = $state(false);

	// ── Q7 Request inbox (M13 Part B) — a stranger's DM is quarantined here, never merged into the
	//    conversation list. `viewingRequests` selects the Requests section in the right pane;
	//    `selectedRequest` (once set) drills into one sender's bucket.
	let viewingRequests = $state(false);
	let selectedRequest: DmRequestView | null = $state(null);


	// ── Compose-to-npub (spec §9 first-contact deep link from Discovery) ─────────────────────────
	let composeOpen = $state(false);
	let composeTo = $state('');
	let composeBody = $state('');
	let composeSending = $state(false);

	async function loadTopics() {
		try { topics = await topicList(); } catch { /* relay unreachable */ }
	}

	async function loadRequests() {
		try { dmRequests.set(await fetchDmRequests()); } catch { /* relay/store unreachable */ }
	}

	async function loadChannel(topicId: string) {
		try {
			const view = await topicChannel(topicId);
			channelPosts = sortChannelPostsAscending(view.posts);
			channelAnnouncements = view.announcements;
			// devtest #2: reading the channel clears its Topics nav badge — advance the seen watermark to
			// the newest announcement and mirror it into the store so the badge updates without a refetch.
			const newest = view.announcements.reduce((m, a) => Math.max(m, a.ts), 0);
			if (newest > 0) {
				announceSeen.update((s) => (newest > (s[topicId] ?? 0) ? { ...s, [topicId]: newest } : s));
				topicAnnounceMarkSeen(topicId, newest).catch(() => { /* non-fatal — reseeds next launch */ });
			}
		} catch { /* relay unreachable */ }
	}

	async function selectTopic(t: TopicView) {
		selectedTopic = t;
		selectedPeer = null;
		viewingRequests = false;
		selectedRequest = null;
		channelPosts = [];
		channelAnnouncements = [];
		await loadChannel(t.topic_id);
		await tick();
		scrollToBottom();
	}

	function openRequests() {
		viewingRequests = true;
		selectedRequest = null;
		selectedPeer = null;
		selectedTopic = null;
	}

	function openRequest(r: DmRequestView) {
		selectedRequest = r;
	}

	// Petname-dialog wiring (M13 W5 Slice 2): accepting a Request now asks for an optional petname +
	// group first, via the same shared AddContactDialog used on Contacts, instead of always passing
	// `null` straight through to `dmRequestAccept`.
	let acceptDialogOpen = $state(false);
	let acceptTarget: DmRequestView | null = $state(null);
	let createGroupOpen = $state(false);
	let groups: Group[] = $state([]);

	async function loadGroups() {
		try { groups = await groupsGet(); } catch { /* non-fatal */ }
	}

	function openAcceptDialog(r: DmRequestView) {
		acceptTarget = r;
		acceptDialogOpen = true;
	}

	async function handleCreateGroup(detail: { name: string; color: string; trusted: boolean }) {
		const { name, color, trusted } = detail;
		try {
			await groupsCreate(name, color);
			if (trusted) await groupsSetTrusted(name, true);
			await loadGroups();
		} catch (e) { toast(String(e), 'error'); }
	}

	async function completeAccept(r: DmRequestView, petname: string | null, group: string | null) {
		try {
			const drained = await dmRequestAccept(r.npub, petname);
			inboxMessages.update((prev) => {
				const seenKeys = new Set(prev.map((m) => `${m.from}|${m.sent_at}`));
				const fresh = drained.filter((m) => !seenKeys.has(`${m.from}|${m.sent_at}`));
				return [...prev, ...fresh];
			});
			dmRequests.update((prev) => prev.filter((x) => x.npub !== r.npub));
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			if (group) {
				try {
					await contactUpdateGroups(r.npub, [group]);
					contacts.set(await getContacts());
				} catch { /* non-fatal */ }
			}
			viewingRequests = false;
			selectedRequest = null;
			const peer = $contacts.find((c) => c.npub === r.npub);
			if (peer) await selectPeer(peer);
			toast('Contact added', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleAcceptSave(detail: { petname: string; group: string | null }) {
		if (!acceptTarget) return;
		const r = acceptTarget;
		acceptDialogOpen = false;
		acceptTarget = null;
		await completeAccept(r, detail.petname, detail.group);
	}

	async function handleAcceptSkip() {
		if (!acceptTarget) return;
		const r = acceptTarget;
		acceptDialogOpen = false;
		acceptTarget = null;
		await completeAccept(r, null, null);
	}

	async function handleDecline(r: DmRequestView) {
		try {
			await dmRequestDecline(r.npub);
			dmRequests.update((prev) => prev.filter((x) => x.npub !== r.npub));
			selectedRequest = null;
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function handleBlock(r: DmRequestView) {
		try {
			await dmBlock(r.npub);
			dmRequests.update((prev) => prev.filter((x) => x.npub !== r.npub));
			selectedRequest = null;
			toast('Blocked', 'success');
		} catch (e) {
			toast(String(e), 'error');
		}
	}

	async function sendChannelPost() {
		if (!selectedTopic || !channelDraft.trim() || channelSending) return;
		channelSending = true;
		const body = channelDraft.trim();
		channelDraft = '';
		try {
			await topicPost(selectedTopic.topic_id, body);
			await loadChannel(selectedTopic.topic_id);
			await tick();
			scrollToBottom();
		} catch (e) {
			toast(String(e), 'error');
			channelDraft = body;
		} finally {
			channelSending = false;
		}
	}

	function channelKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendChannelPost(); }
	}

	// M13 Part B (Q7): the conversation list is contacts ONLY — a stranger's DM no longer merges in
	// here at all (the request pane, above, replaces the old inboxOnlyPeers merge). M15 W6 replaced
	// the inline latestMessageTime with chat-preview.ts's peerPreview (time + text in one pass).


	// Cache of display_name for npubs not in $contacts (Request-bucket senders). Populated lazily by
	// fetchNonContactNames(); never causes re-triggers because we only write when a key is absent.
	let peerNameCache: Record<string, string> = {};
	const fetchingNames = new Set<string>(); // prevents duplicate in-flight fetches

	async function fetchNonContactNames(npubs: string[]) {
		for (const npub of npubs) {
			if (fetchingNames.has(npub) || peerNameCache[npub]) continue;
			fetchingNames.add(npub);
			try {
				const fetched = await pasteKey(npub);
				if (fetched.profile?.display_name) {
					peerNameCache = { ...peerNameCache, [npub]: fetched.profile.display_name };
				}
			} catch { /* relay unreachable or peer has no profile — fall back to shortId */ }
		}
	}


	// Resolve display name for a sender hb_id — contacts first, then fetched cache.
	function senderName(hb_id: string): string {
		if (hb_id === myId) return 'You';
		const contact = $contacts.find(c => c.npub === hb_id);
		// Petname-first via the shared helper (M13 W5); cache/shortId fallbacks unchanged.
		if (contact && (contact.petname?.trim() || contact.profile?.display_name)) return contactDisplayName(contact);
		if (peerNameCache[hb_id]) return peerNameCache[hb_id];
		return shortId(hb_id);
	}

	onMount(() => {
		refreshInbox();
		loadGroups();

		// Discovery first-contact deep link (spec §9): `/chat?compose=<npub-or-sharecode>` prefills
		// and opens the compose modal.
		const composeParam = $page.url.searchParams.get('compose');
		if (composeParam) {
			composeTo = composeParam;
			composeOpen = true;
		}

		// Topic channel deep link (devtest #15): `/chat?topic=<topic_id>` from the Topics page's
		// "Open this Topic's channel in Chat" link selects the channel directly, no second click.
		const topicParam = $page.url.searchParams.get('topic');
		loadTopics().then(() => {
			if (!topicParam) return;
			const t = resolveTopicParam(topicParam, topics);
			if (t) selectTopic(t);
			else toast("That Topic channel isn't available — you may have left it.", 'error');
		});

		// Local DM poll while the chat page is open. M12 W1 Decision B: backed off from 4 s (the
		// dominant connect source against the relays) and visibility-gated — paused while the window
		// is hidden so it doesn't churn relay connections in the background; resumes on show.
		let pollTick = 0;
		const fastPoll = setInterval(async () => {
			if (!$identity || document.hidden) return;
			// devtest v0.12.4 #1: DMs poll every 3s (cheap incremental fetch), but the open Topic
			// channel's low-velocity 24h posts refresh on a slower cadence so we don't over-poll the relay.
			pollTick++;
			if (selectedTopic && pollTick % CHANNEL_REFRESH_EVERY_TICKS === 0) loadChannel(selectedTopic.topic_id);
			try {
				const msgs = await getMessages();
				// Detect genuinely new messages for the selected peer and auto-scroll.
				if (selectedPeer) {
					const prevCount = $inboxMessages.filter(m => m.from === selectedPeer!.npub).length;
					const nextCount = msgs.filter(m => m.from === selectedPeer!.npub).length;
					if (nextCount > prevCount) {
						inboxMessages.set(msgs);
						// The open conversation just got new messages — re-advance its watermark too,
						// so an open thread never accumulates a phantom unread count (devtest #16).
						const latest = latestFromPeer(msgs, selectedPeer.npub);
						if (latest) {
							readWatermarks.update((w) => ({ ...w, [selectedPeer!.npub]: latest }));
							advanceReadWatermark(selectedPeer.npub, latest).catch(() => { });
						}
						await tick();
						scrollToBottom();
						return;
					}
				}
				inboxMessages.set(msgs);
			} catch { /* relay unreachable */ }
			// Q7: refresh the Request inbox right after the main inbox poll.
			loadRequests();
		}, DM_POLL_VISIBLE_MS);

		return () => {
			clearInterval(fastPoll);
		};
	});

	async function refreshInbox() {
		if (!$identity) return;
		loading = true;
		try {
			const msgs = await getMessages();
			inboxMessages.set(msgs);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			loading = false;
		}
		loadRequests();
	}

	async function selectPeer(peer: CachedPeer) {
		selectedPeer = peer;
		selectedTopic = null;
		viewingRequests = false;
		selectedRequest = null;
		// Opening a conversation reads it: advance the peer's watermark to the newest message we
		// have from them (devtest #16 — the badge clears per-conversation, not on merely landing on
		// /chat). Optimistic local update + best-effort persist.
		const latest = latestFromPeer($inboxMessages, peer.npub);
		if (latest) {
			readWatermarks.update((w) => ({ ...w, [peer.npub]: latest }));
			advanceReadWatermark(peer.npub, latest).catch(() => { });
		}
		await tick();
		scrollToBottom();
	}

	async function handleSend() {
		if (!selectedPeer || !draft.trim() || sending) return;
		sending = true;
		const content = draft.trim();
		draft = '';
		try {
			const sent = await sendMessage(selectedPeer.npub, content);
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

	// Compose-to-npub modal (spec §9): send() rebuilds a CachedPeer stub if the recipient wasn't
	// already a contact, so the composer can select straight into the new conversation.
	async function handleComposeSend() {
		const to = composeTo.trim();
		const content = composeBody.trim();
		if (!to || !content || composeSending) return;
		composeSending = true;
		try {
			const sent = await sendMessage(to, content);
			sentMessages.update((prev) => [...prev, sent]);
			composeOpen = false;
			composeTo = '';
			composeBody = '';
			try { contacts.set(await getContacts()); } catch { /* non-fatal */ }
			const peer = $contacts.find((c) => c.npub === sent.to) ?? ({
				npub: sent.to, browse_key_hex: undefined, petname: undefined, profile: undefined,
				collections: [], online: false, last_fetched: '', local_tags: [],
			} satisfies CachedPeer);
			await selectPeer(peer);
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			composeSending = false;
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


	function viewProfile(peer: CachedPeer) {
		goto('/contacts');
	}

	let sortedRequests = $derived(sortRequests($dmRequests));
	let requestCount = $derived(requestBadge($dmRequests));
	let myId = $derived($identity?.npub ?? '');
	// M15 W6: only contacts with actual DM history appear in the list (messageless contacts are
	// reachable via compose + the Contacts tab; the pane still renders from selectedPeer, and a new
	// thread's row appears after the first send). Sorted newest-first by the latest message's ISO time.
	let historyPeers = $derived(peersWithHistory($inboxMessages, $sentMessages));
	let allConversationPeers = $derived([...$contacts]
		.filter((c) => historyPeers.has(c.npub))
		.sort((a, b) => {
			// Sort by parsed ms (not string compare) so a non-UTC-offset timestamp can't misorder the
			// list (chorus catch — removes the "backend always emits UTC-Z" assumption).
			const aT = new Date(peerPreview($inboxMessages, $sentMessages, a.npub)?.time ?? 0).getTime();
			const bT = new Date(peerPreview($inboxMessages, $sentMessages, b.npub)?.time ?? 0).getTime();
			return bT - aT; // newest first
		}));
	// Wires the search box (devtest copy audit — it was dead): filters the visible rows only, never
	// the underlying stores.
	let visiblePeers = $derived(filterConversations(allConversationPeers, searchQuery, senderName));
	let visibleTopics = $derived(filterTopics(topics, searchQuery));
	let conversation = $derived(selectedPeer
		? [
				...$inboxMessages.filter((m) => m.from === selectedPeer!.npub),
				...$sentMessages.filter((m) => m.to === selectedPeer!.npub)
			].sort((a, b) => a.sent_at.localeCompare(b.sent_at))
		: []);
	// Eagerly fetch names for Request-bucket senders whenever the list changes.
	$effect(() => {
		fetchNonContactNames($dmRequests.map((r) => r.npub));
	});
	// devtest v0.12.1 #4: resolve the `?peer=<npub>` deep-link through selectPeer once the contact is
	// loaded. Re-runs when $contacts settles; a contact double-clicked in Contacts is always present,
	// so it opens their conversation pane (empty-thread until the first message, then it joins the list).
	$effect(() => {
		const npub = $page.url.searchParams.get('peer') ?? '';
		if (!npub || npub === peerDeepLinked) return;
		const peer = $contacts.find((c) => c.npub === npub);
		if (peer) {
			peerDeepLinked = npub;
			selectPeer(peer);
		}
	});
	// devtest #16: derived straight from the persisted per-peer watermark, replacing the old
	// seenCounts in-memory snapshot (which reset to "no unread" on every remount).
	let unreadCounts = $derived(unreadByPeer($inboxMessages, $readWatermarks, myId));
	// Show a privacy notice if the selected peer is not in contacts (may have DMs restricted).
	let selectedIsContact = $derived(selectedPeer ? $contacts.some(c => c.npub === selectedPeer!.npub) : false);
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
				<div class="header-icons">
					<button class="icon-btn" onclick={() => (composeOpen = true)} title="New message">
						{@html icons.plus}
					</button>
					<button class="icon-btn" onclick={refreshInbox} disabled={loading} title="Refresh inbox">
						{@html icons.refresh}
					</button>
				</div>
			</div>
			<div class="convo-search">
				<div class="search-wrap">
					<span class="search-icon-sm">{@html icons.search}</span>
					<input class="search-bare" type="text" placeholder="Search…" bind:value={searchQuery} />
				</div>
			</div>
			<div class="convo-list">
				{#if visibleTopics.length > 0}
					<div class="convo-section-label">Channels</div>
					{#each visibleTopics as t (t.topic_id)}
						<button class="convo-item" class:convo-active={selectedTopic?.topic_id === t.topic_id} onclick={() => selectTopic(t)}>
							<div class="channel-hash">#</div>
							<div class="convo-info">
								<div class="convo-row">
									<span class="convo-name" class:convo-name-active={selectedTopic?.topic_id === t.topic_id}>{t.name}</span>
									{#if t.private}<span class="convo-lock" title="Private topic">🔒</span>{/if}
								</div>
							</div>
						</button>
					{/each}
				{/if}
				{#if $dmRequests.length > 0}
					<div class="convo-section-label">Requests</div>
					<button class="convo-item" class:convo-active={viewingRequests} onclick={openRequests}>
						<div class="channel-hash">🔔</div>
						<div class="convo-info">
							<div class="convo-row">
								<span class="convo-name" class:convo-name-active={viewingRequests}>Message requests</span>
								<span class="unread-badge">{requestCount}</span>
							</div>
						</div>
					</button>
				{/if}
				<div class="convo-section-label">Direct messages</div>
				{#if visiblePeers.length === 0}
					<div class="convo-empty">
						{allConversationPeers.length === 0 ? 'No conversations yet — add someone in Contacts to start one.' : 'No matches.'}
					</div>
				{:else}
					{#each visiblePeers as peer}
						{@const name = senderName(peer.npub)}
						{@const initial = name[0]?.toUpperCase() ?? '?'}
						{@const hue = avatarHue(initial)}
						{@const unread = unreadCounts[peer.npub] ?? 0}
						{@const active = selectedPeer?.npub === peer.npub}
						{@const preview = peerPreview($inboxMessages, $sentMessages, peer.npub)}
						<button class="convo-item" class:convo-active={active} onclick={() => selectPeer(peer)}>
							<Avatar letter={initial} size={34} {hue} picture={peer.profile?.picture} />
							<div class="convo-info">
								<div class="convo-row">
									<span class="convo-name" class:convo-name-active={active}>{name}</span>
									{#if preview}<span class="convo-time">{relativeTime(preview.time, new Date())}</span>{/if}
								</div>
								<div class="convo-preview-row">
									{#if preview}<span class="convo-preview-text">{preview.text}</span>{/if}
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
			{#if selectedTopic}
				<!-- Topic channel: a persistent entry (your durable membership, §11) whose posts are
				     24h-ephemeral (server NIP-40 + the local filter in topic_channel). -->
				<div class="pane-header">
					<div class="channel-hash channel-hash-lg">#</div>
					<div class="pane-peer-info">
						<div class="pane-peer-row">
							<span class="pane-peer-name">{selectedTopic.name}</span>
							{#if selectedTopic.private}<span class="pill pill-offline">private</span>{/if}
						</div>
						<span class="channel-sub">Topic channel · posts wipe after 24h · manage in Topics</span>
					</div>
				</div>

				<div class="thread" bind:this={threadEl}>
					{#if channelItems.length === 0}
						<p class="thread-empty">No posts in the last 24h. Say something!</p>
					{:else}
						{#each channelItems as item (item.kind + '|' + (item.kind === 'post' ? item.post.author_npub + '|' + item.post.ts : item.announce.author_npub + '|' + item.announce.ts))}
							{#if item.kind === 'announce'}
								<div class="announce-banner">
									<span class="announce-icon">📣</span>
									<div class="announce-body">
										<span class="announce-author">{senderName(item.announce.author_npub)}</span>
										<p class="announce-text">{item.announce.body}</p>
										<span class="announce-time">{formatTime(new Date(item.announce.ts * 1000).toISOString())}</span>
									</div>
								</div>
							{:else}
								{@const isMe = item.post.author_npub === myId}
								<div class="bubble-wrap" class:bubble-me={isMe}>
									<div class="bubble" class:bubble-sent={isMe} class:bubble-recv={!isMe}>
										{#if !isMe}<span class="bubble-author">{senderName(item.post.author_npub)}</span>{/if}
										<p class="bubble-text">{item.post.body}</p>
										<span class="bubble-time">{formatTime(new Date(item.post.ts * 1000).toISOString())}</span>
									</div>
								</div>
							{/if}
						{/each}
					{/if}
				</div>

				<div class="composer">
					<div class="compose-box">
						<textarea
							class="compose-input"
							placeholder="Message #{selectedTopic.name}…"
							bind:value={channelDraft}
							onkeydown={channelKeydown}
							disabled={channelSending}
							rows="2"
						></textarea>
						<div class="compose-footer">
							<button class="btn-primary btn-send" onclick={sendChannelPost} disabled={!channelDraft.trim() || channelSending}>
								{channelSending ? '…' : 'Post'} <span>{@html icons.send}</span>
							</button>
						</div>
					</div>
				</div>
			{:else if viewingRequests}
				{#if !selectedRequest}
					<!-- Requests list: sorted newest-activity-first (Q7 — never merged into the main list). -->
					<div class="pane-header">
						<div class="channel-hash channel-hash-lg">🔔</div>
						<div class="pane-peer-info">
							<div class="pane-peer-row"><span class="pane-peer-name">Message requests</span></div>
							<span class="channel-sub">Quarantined until you accept, decline, or block</span>
						</div>
					</div>
					<div class="requests-explainer">{REQUEST_EXPLAINER}</div>
					<div class="thread">
						{#if sortedRequests.length === 0}
							<p class="thread-empty">No message requests.</p>
						{:else}
							{#each sortedRequests as r (r.npub)}
								{@const name = senderName(r.npub)}
								{@const initial = name[0]?.toUpperCase() ?? '?'}
								<button class="request-row" onclick={() => openRequest(r)}>
									<Avatar letter={initial} size={34} hue={avatarHue(initial)} />
									<div class="convo-info">
										<div class="convo-row">
											<span class="convo-name">{name}</span>
											<span class="unread-badge">{r.message_count}</span>
										</div>
										<div class="request-preview">{requestPreview(r)}</div>
										{#if r.fingerprint}
											<div class="request-fp" title="Identity fingerprint — check it before accepting a stranger">
												<span class="request-fp-swatch" style="background:{r.fingerprint.colorHex}"></span>
												{renderFingerprint(r.fingerprint)}
											</div>
										{/if}
									</div>
								</button>
							{/each}
						{/if}
					</div>
				{:else}
					{@const req = selectedRequest}
					{@const reqName = senderName(req.npub)}
					{@const isRequestContact = $contacts.some((c) => c.npub === req.npub)}
					<!-- Opened request: read-only messages + Accept/Decline/Block (no reply until accepted). -->
					<div class="pane-header">
						<Avatar letter={reqName[0]?.toUpperCase() ?? '?'} size={36} hue={avatarHue(reqName[0] ?? '?')} />
						<div class="pane-peer-info">
							<div class="pane-peer-row"><span class="pane-peer-name">{reqName}</span></div>
							<span class="mono">{shortId(req.npub)}</span>
						</div>
						<button class="btn-ghost btn-sm" onclick={() => (selectedRequest = null)}>← Back</button>
					</div>
					<div class="requests-explainer">{REQUEST_EXPLAINER}</div>
					<div class="thread">
						{#each req.messages as msg}
							<div class="bubble-wrap">
								<div class="bubble bubble-recv">
									<p class="bubble-text">{manifestRequestHint(msg.content) ?? msg.content}</p>
									<span class="bubble-time">{formatTime(msg.sent_at)}</span>
								</div>
							</div>
						{/each}
					</div>
					{#if !canReply(isRequestContact)}
						<div class="composer request-actions">
							<button class="btn-primary" onclick={() => openAcceptDialog(req)}>Accept</button>
							<button class="btn-ghost" onclick={() => handleDecline(req)}>Decline</button>
							<button class="btn-ghost btn-danger" onclick={() => handleBlock(req)}>Block</button>
						</div>
					{/if}
				{/if}
			{:else if !selectedPeer}
				<div class="convo-empty-state">
					<p>Select a contact to view the conversation.</p>
					<p class="privacy-note">
						{@html icons.shield} Messages are end-to-end encrypted — relays never see who sent them or what they say.
					</p>
				</div>
			{:else}
				<!-- Header -->
				<div class="pane-header">
					<Avatar
						letter={(selectedPeer.profile?.display_name || selectedPeer.npub)[0].toUpperCase()}
						size={36}
						hue={avatarHue((selectedPeer.profile?.display_name || selectedPeer.npub)[0])}
						picture={selectedPeer.profile?.picture}
					/>
					<div class="pane-peer-info">
						<div class="pane-peer-row">
							<span class="pane-peer-name">{selectedPeer.profile?.display_name || shortId(selectedPeer.npub)}</span>
							{#if selectedPeer.online}
								<span class="pill pill-online"><span class="pill-dot"></span> Online</span>
							{:else}
								<span class="pill pill-offline">Offline</span>
							{/if}
						</div>
						<span class="mono">{shortId(selectedPeer.npub)}</span>
					</div>
					<!-- M15 W6: the always-on E2E banner is consolidated into this header shield (hover) +
					     the empty-thread note; the offline/not-a-contact banners stay (contextual). -->
					<span class="e2e-shield" title="End-to-end encrypted — relays see only that someone messaged this person, never the content or the sender.">{@html icons.shield}</span>
					<button class="btn-ghost btn-sm" onclick={() => { if (selectedPeer) viewProfile(selectedPeer); }}>View profile</button>
				</div>

				<!-- Offline notice -->
				{#if !selectedPeer.online}
					<div class="offline-banner">
						<span class="offline-dot"></span>
						<span>{selectedPeer.profile?.display_name || shortId(selectedPeer.npub)} is offline — they'll see your message the next time they open Hoardbook.</span>
					</div>
				{/if}

				<!-- Notice for message requests (sender not in recipient's contacts) -->
				{#if !selectedIsContact}
					<div class="request-banner">
						<span>This person may not have added you back — their privacy settings may filter your messages.</span>
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
									<div class="day-line"></div>
									<span class="day-label">{formatDate(msg.sent_at)}</span>
									<div class="day-line"></div>
								</div>
							{/if}
							<div class="bubble-wrap" class:bubble-me={isMe}>
								<div class="bubble" class:bubble-sent={isMe} class:bubble-recv={!isMe}>
									<p class="bubble-text">{manifestRequestHint(msg.content) ?? msg.content}</p>
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
							onkeydown={handleKeydown}
							disabled={sending}
							rows="2"
						></textarea>
						<div class="compose-footer">
							<button
								class="btn-primary btn-send"
								onclick={handleSend}
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

<!-- Petname + group dialog shown before accepting a Request (M13 W5 Slice 2). -->
<AddContactDialog
	open={acceptDialogOpen}
	displayName={acceptTarget ? senderName(acceptTarget.npub) : ''}
	{groups}
	onsave={handleAcceptSave}
	onskip={handleAcceptSkip}
	onnewGroup={() => (createGroupOpen = true)}
	oncancel={() => { acceptDialogOpen = false; acceptTarget = null; }}
/>
<CreateGroupDialog open={createGroupOpen} oncreate={handleCreateGroup} oncancel={() => (createGroupOpen = false)} />

<!-- Compose-to-npub (spec §9 first-contact deep link) — a + icon-btn beside refresh opens this. -->
<Modal open={composeOpen} title="New message" onclose={() => (composeOpen = false)}>
	<div class="compose-fields">
		<input placeholder="npub or hbk share code…" bind:value={composeTo} />
		{#if composeTo.trim() && isComposeToSelf(composeTo, $identity?.npub ?? '', $identity?.share_code ?? '')}
			<div class="compose-hint">That's your own ID.</div>
		{:else if composeTo.trim() && composeRecipientKind(composeTo) === 'invalid'}
			<div class="compose-hint">Doesn't look like an npub or share code — sending will reject it if it's wrong.</div>
		{/if}
		<textarea class="compose-modal-input" placeholder="Message…" bind:value={composeBody} rows="3"></textarea>
	</div>
	{#snippet actions()}
		<button class="btn-ghost" onclick={() => (composeOpen = false)}>Cancel</button>
		<button class="btn-primary" disabled={!composeTo.trim() || !composeBody.trim() || composeSending || isComposeToSelf(composeTo, $identity?.npub ?? '', $identity?.share_code ?? '')} onclick={handleComposeSend}>
			{composeSending ? '…' : 'Send'}
		</button>
	{/snippet}
</Modal>

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

	.header-icons { display: flex; gap: 4px; align-items: center; }

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

	/* M15 W7: removed the dead .convo-divider rule (unreferenced). */

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

	/* devtest v0.12.1 #5: a private-topic marker reads as a lock, not a filled dot (the old amber dot
	   looked like a permanent unread/notification bubble). */
	.convo-lock { font-size: 10px; line-height: 1; flex-shrink: 0; opacity: 0.6; }

	/* Topic channels (M11) */
	.convo-section-label {
		padding: 10px 12px 4px;
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 1px;
		color: var(--fg-dim);
	}
	.channel-hash {
		width: 34px; height: 34px; flex-shrink: 0;
		display: flex; align-items: center; justify-content: center;
		border-radius: 8px;
		background: var(--bg-elev2);
		color: var(--fg-muted);
		font-size: 17px; font-weight: 700;
	}
	.channel-hash-lg { width: 36px; height: 36px; font-size: 18px; }
	.channel-sub { font-family: var(--font-mono); font-size: 11px; color: var(--fg-dim); }
	.bubble-author { display: block; font-size: 10.5px; font-weight: 600; color: var(--accent); margin-bottom: 2px; }

	.convo-preview-row { display: flex; align-items: center; margin-top: 2px; gap: 4px; }
	/* M15 W6: last-message preview + relative time in the conversation list. */
	.convo-time { font-size: 10.5px; color: var(--fg-dim); flex-shrink: 0; font-feature-settings: 'tnum'; }
	.convo-preview-text { font-size: 11.5px; color: var(--fg-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }

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

	/* M15 W6: always-on privacy banner replaced by this subtle header shield (hover) + empty-thread note. */
	.e2e-shield { color: var(--accent); display: flex; margin-left: 4px; cursor: help; flex-shrink: 0; }
	.e2e-shield :global(svg) { width: 15px; height: 15px; }

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

	/* M15 W1: buttons unified on the app.css .btn system (local copies removed; .btn-send stays,
	   it's a chat-specific composer button not in the shared vocabulary). */

	/* Topic announcements (M13 Part A) — a highlighted, read-only broadcast above the ordinary posts. */
	.announce-banner {
		display: flex;
		gap: 10px;
		padding: 10px 14px;
		margin-bottom: 10px;
		background: var(--accent-soft);
		border: 1px solid var(--border);
		border-radius: 9px;
	}
	.announce-icon { font-size: 15px; line-height: 1; }
	.announce-body { flex: 1; min-width: 0; }
	.announce-author { display: block; font-size: 10.5px; font-weight: 600; color: var(--accent); margin-bottom: 2px; }
	.announce-text { font-size: 13px; line-height: 1.5; white-space: pre-wrap; word-break: break-word; margin: 0; }
	.announce-time { font-size: 10px; color: var(--fg-dim); display: block; margin-top: 3px; }

	/* Q7 Request inbox */
	.requests-explainer {
		padding: 8px 18px;
		background: var(--accent-soft);
		border-bottom: 1px solid var(--border);
		font-size: 11.5px;
		color: var(--fg-muted);
	}
	.request-row {
		width: 100%;
		display: flex;
		gap: 10px;
		align-items: flex-start;
		padding: 10px;
		background: transparent;
		border: none;
		border-bottom: 1px solid var(--divider);
		border-radius: 7px;
		cursor: pointer;
		color: inherit;
		font-family: inherit;
		text-align: left;
	}
	.request-row:hover { background: var(--bg-elev1); }
	.request-preview { font-size: 12px; color: var(--fg-muted); margin-top: 2px; }
	.request-fp {
		display: flex; align-items: center; gap: 5px;
		font-family: var(--font-mono); font-size: 10.5px; color: var(--fg-dim);
		margin-top: 4px;
	}
	.request-fp-swatch { width: 9px; height: 9px; border-radius: 50%; flex-shrink: 0; }
	.request-actions { display: flex; gap: 8px; justify-content: flex-end; }

	/* Compose-to-npub modal */
	.compose-hint { font-size: 11px; color: var(--fg-dim); }
	.compose-modal-input {
		width: 100%;
		padding: 8px 10px;
		font-family: var(--font-ui);
		font-size: 13px;
		color: var(--fg);
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 7px;
		resize: none;
		box-sizing: border-box;
	}
	/* M15 W2: compose modal now uses Modal.svelte; only the field layout + input styling are local. */
	.compose-fields { display: flex; flex-direction: column; gap: 8px; }
	.compose-fields input {
		padding: 6px 9px; background: var(--bg-elev2); color: var(--fg);
		border: 1px solid var(--border); border-radius: 6px; font: inherit;
	}
</style>
