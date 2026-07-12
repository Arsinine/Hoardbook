<script lang="ts">
	// "+ Add contact" modal (devtest #17/#18 phonebook redesign) — the lookup-by-ID card + §6 Discover
	// section, MOVED here verbatim from contacts/+page.svelte (byte-for-byte row/card markup) so both
	// existing add entry points keep working unchanged: a lookup-card "Add contact" and a Discover-hit
	// "Add contact" both call `onadd`, which the page routes into its existing `openAddContact` →
	// AddContactDialog → completeFollow funnel (petname + group picker, then `follow`).
	import { pasteKey, searchPeers, type PeerSearchHit } from '../api.js';
	import { contacts, identity, toast } from '../stores.js';
	import { icons, avatarHue } from '../icons.js';
	import Avatar from './Avatar.svelte';
	import FeatureTooltip from './FeatureTooltip.svelte';
	import Modal from './Modal.svelte';
	import type { CachedPeer } from '../types.js';
	import { renderFingerprint } from '../identity-display.js';
	import { DISCOVER_CONTENT_TYPES, parseTagInput, canSearch, toggleContentType } from '../discover-view.js';

	interface Props {
		open?: boolean;
		// `code` is what `follow` must re-resolve — the full `hbk1…` share code (carrying the
		// browse-key) for a lookup, or the bare npub for a discovery hit. Passing only the npub
		// (as before) silently dropped the key and made every added contact keyless (devtest #3).
		onadd?: (code: string, npub: string, displayName: string) => void;
		onclose?: () => void;
	}

	let { open = false, onadd, onclose }: Props = $props();

	// Lookup state
	let input = $state('');
	let loading = $state(false);
	let result = $state<CachedPeer | null>(null);
	// The exact string that produced `result` — threaded to `follow` so the browse-key survives the
	// add (devtest #3). Captured at lookup time so a later edit to `input` can't desync it.
	let lookedUpCode = $state('');

	let existingContact = $derived($contacts.find((c) => c.npub === result?.npub));
	let alreadyFollowed = $derived(!!existingContact);
	// devtest #4: a contact added by npub/discovery is keyless. Pasting their FULL share code later
	// must be allowed to attach the browse-key (re-adding overwrites the stored contact) — otherwise
	// the "Added"/disabled button dead-ends the upgrade and they stay permanently unbrowseable.
	let canUnlock = $derived(!!result?.browse_key_hex && !!existingContact && !existingContact.browse_key_hex);

	async function handleLookup() {
		const id = input.trim();
		if (!id) return;
		// devtest #14 self-guard — the same exact-match check the page used to run inline.
		if (id === $identity?.npub) {
			toast("That's your own ID — you can't add yourself as a contact.", 'error');
			return;
		}
		loading = true;
		result = null;
		try {
			result = await pasteKey(id);
			lookedUpCode = id;
		} catch (e) {
			toast(String(e), 'error');
		} finally {
			loading = false;
		}
	}

	function handleFollow() {
		if (!result) return;
		onadd?.(lookedUpCode, result.npub, result.profile?.display_name ?? '');
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') handleLookup();
	}

	// ── §6 Discovery (moved from Browse — devtest 2026-06-25 #6) ─────────────────────────────────
	let discoverOpen = $state(false);
	let discoverTags = $state('');
	let discoverTypes: string[] = $state([]);
	let discoverResults: PeerSearchHit[] = $state([]);
	let discovering = $state(false);
	let discoverError = $state('');
	let discovered = $state(false); // a search has run at least once (drives the empty-vs-no-results copy)
	let parsedDiscoverTags = $derived(parseTagInput(discoverTags));
	let canDiscover = $derived(canSearch(parsedDiscoverTags, discoverTypes));

	async function runDiscover() {
		if (!canDiscover) { discoverError = 'Enter at least one tag or content type to search.'; return; }
		discovering = true;
		discoverError = '';
		try {
			discoverResults = await searchPeers(parsedDiscoverTags, discoverTypes);
			discovered = true;
		} catch (e) {
			discoverError = String(e);
		} finally {
			discovering = false;
		}
	}

	function followHit(hit: PeerSearchHit) {
		// bare npub only: awareness, NOT a browse-key (INV-2) — the dialog's Skip path preserves that.
		// Discovery hits are teaser-only (DISC3) — no browse-key exists, so the code IS the npub.
		onadd?.(hit.npub, hit.npub, hit.display_name);
	}

	function close() {
		onclose?.();
	}
</script>

<Modal open={open} width="520px" padding="0" onclose={close}>
	<div class="acp-frame">
		<div class="acp-modal-head">
			<h2>Add contact</h2>
			<button type="button" class="acp-close" aria-label="Close" onclick={close}>{@html icons.close}</button>
		</div>
		<div class="acp-body">
				<!-- Lookup section -->
				<div class="lookup-section">
					<div class="lookup-label">Look up a peer by ID</div>
					<div class="search-row">
						<div class="search-input-wrap">
							<span class="search-icon">{@html icons.search}</span>
							<input
								class="search-input hb-mono"
								type="text"
								placeholder="npub1… or share code (hbk1…)"
								bind:value={input}
								onkeydown={handleKeydown}
							/>
						</div>
						<button class="btn-primary" onclick={handleLookup} disabled={!input.trim() || loading}>
							{loading ? 'Looking up…' : 'Lookup'}
						</button>
					</div>

					{#if result}
						<div class="result">
							<div class="profile-card">
								<div class="profile-banner"></div>
								<div class="profile-inner">
									<div class="profile-top">
										<Avatar
											letter={(result.profile?.display_name || result.npub)[0].toUpperCase()}
											size={52}
											hue={avatarHue((result.profile?.display_name || result.npub)[0])}
											picture={result.profile?.picture}
										/>
										<div class="profile-name-col">
											<div class="name-row">
												<span class="peer-name">{result.profile?.display_name || 'Unknown'}</span>
												{#if result.online}
													<span class="pill pill-online"><span class="pill-dot"></span> Online</span>
												{:else}
													<span class="pill pill-offline">Offline</span>
												{/if}
											</div>
											<span class="mono">{result.npub.slice(0, 18)}…{result.npub.slice(-4)}</span>
										</div>
										<div class="profile-actions">
											<button
												class="btn-primary btn-sm"
												onclick={handleFollow}
												disabled={alreadyFollowed && !canUnlock}
											>
												{canUnlock ? 'Unlock browsing' : alreadyFollowed ? 'Added' : 'Add contact'}
											</button>
										</div>
									</div>

									{#if result.profile?.bio}
										<p class="peer-bio">{result.profile.bio}</p>
									{/if}

									<!-- §7 impersonation fingerprint — your at-a-glance trust check for a stranger you
									     just looked up (bound to the npub, not the display name). -->
									{#if result.fingerprint}
										<div class="fp-row">
											<span class="fp-swatch" style="background:{result.fingerprint.colorHex}"></span>
											<span class="fp-words hb-mono">{result.fingerprint.words.join(' ')} {result.fingerprint.colorHex}</span>
											<FeatureTooltip key="fingerprint" />
										</div>
									{/if}

									<!-- Content types + tags are the only rich fields a public teaser carries, so they
									     are what a lookup can actually show (§4/§5). -->
									{#if (result.profile?.content_types?.length ?? 0) > 0}
										<div class="badge-row-sm">
											{#each result.profile?.content_types ?? [] as ct (ct)}
												<span class="ct-badge">{ct}</span>
											{/each}
										</div>
									{/if}
									{#if (result.profile?.tags?.length ?? 0) > 0}
										<div class="peer-tags">
											{#each result.profile?.tags ?? [] as tag (tag)}
												<span class="peer-tag">{tag}</span>
											{/each}
										</div>
									{/if}
								</div>
							</div>
						</div>
					{/if}
				</div>

				<!-- §6 Discover hoarders (moved from Browse — devtest 2026-06-25 #6). Collapsible so it doesn't
				     clutter the panel; results are the opt-in public teaser only (listings stay 🔒 locked). -->
				<div class="discover-section">
					<button class="discover-toggle" onclick={() => (discoverOpen = !discoverOpen)} aria-expanded={discoverOpen}>
						<span class="discover-toggle-label">{@html icons.search} Discover hoarders</span>
						<span class="discover-chevron" class:open={discoverOpen}>{@html icons.chevronDown}</span>
					</button>
					{#if discoverOpen}
						<div class="discover-body">
							<div class="discover-sub">Search public profiles by tag &amp; content type. Only what people chose to announce is searchable — everyone's listings stay encrypted.</div>
							<div class="ct-row">
								{#each DISCOVER_CONTENT_TYPES as ct (ct.value)}
									<button type="button" class="ct-chip" class:ct-on={discoverTypes.includes(ct.value)}
										onclick={() => (discoverTypes = toggleContentType(discoverTypes, ct.value))}>{ct.label}</button>
								{/each}
							</div>
							<form class="disc-tag-row" onsubmit={(e) => { e.preventDefault(); runDiscover(); }}>
								<input class="disc-tag-input" placeholder="tags (e.g. anime, vhs)" bind:value={discoverTags} />
								<button class="btn-primary btn-sm" type="submit" disabled={!canDiscover || discovering}>
									{discovering ? 'Searching…' : 'Search'}
								</button>
							</form>
							{#if discoverError}<div class="discover-error">{discoverError}</div>{/if}
							{#if discovering}
								<div class="discover-empty">Searching the relays…</div>
							{:else if discovered && discoverResults.length === 0}
								<div class="discover-empty">No hoarders matched those filters.</div>
							{:else if discovered}
								<div class="discover-results">
									{#each discoverResults as hit (hit.npub)}
										{@const letter = (hit.display_name?.[0] ?? hit.npub[0]).toUpperCase()}
										<div class="hit-card">
											<div class="hit-top">
												<Avatar {letter} size={30} hue={avatarHue(letter)} picture={hit.picture ?? undefined} />
												<div class="hit-id">
													<span class="hit-name">{hit.display_name || hit.npub.slice(0, 12) + '…'}</span>
													<span class="hit-stranger" title="Verify the fingerprint before trusting a stranger">unverified — not in your contacts</span>
												</div>
												<button class="hit-follow" onclick={() => followHit(hit)}>Add contact</button>
											</div>
											{#if hit.bio}<div class="hit-bio">{hit.bio}</div>{/if}
											{#if hit.fingerprint}
												<div class="hit-fp" title="Identity fingerprint — check it before trusting a stranger">
													<span class="hit-fp-swatch" style="background:{hit.fingerprint.colorHex}"></span>
													{renderFingerprint(hit.fingerprint)}
												</div>
											{/if}
											{#if hit.content_types.length > 0 || hit.tags.length > 0}
												<div class="hit-tags">
													{#each hit.content_types as ct}<span class="hit-tag hit-tag-ct">{ct}</span>{/each}
													{#each hit.tags.slice(0, 6) as t}<span class="hit-tag">#{t}</span>{/each}
												</div>
											{/if}
											<div class="hit-locked">🔒 Listings locked<FeatureTooltip key="listings-locked" /></div>
										</div>
									{/each}
								</div>
							{:else}
								<div class="discover-empty">Pick a content type or enter a tag, then Search.</div>
							{/if}
						</div>
					{/if}
				</div>
			</div>
		</div>
</Modal>
<!-- /M15 W2: AddContactPanel now wraps its head+body in Modal.svelte -->

<style>
	/* M15 W2: backdrop/card now come from Modal.svelte (base level; the petname + New-group dialogs
	   are `stacked`, so they still sit above this panel). This frame just lays out head + body. */
	.acp-frame {
		display: flex; flex-direction: column;
		max-height: min(680px, calc(100vh - 60px));
	}
	.acp-modal-head {
		display: flex; align-items: center; justify-content: space-between;
		padding: 16px 18px; border-bottom: 1px solid var(--border);
		flex-shrink: 0;
	}
	.acp-modal-head h2 { font-size: 15px; font-weight: 600; margin: 0; }
	.acp-close {
		background: transparent; border: none; cursor: pointer;
		color: var(--fg-muted); display: flex; padding: 4px;
	}
	.acp-close:hover { color: var(--fg); }
	.acp-body { padding: 18px; overflow-y: auto; display: flex; flex-direction: column; gap: 16px; }

	/* Lookup */
	.lookup-section { display: flex; flex-direction: column; }

	.lookup-label {
		font-size: 10.5px;
		color: var(--fg-dim);
		text-transform: uppercase;
		letter-spacing: 1.2px;
		font-weight: 600;
		margin-bottom: 10px;
	}

	.search-row { display: flex; gap: 8px; margin-bottom: 16px; }

	.search-input-wrap {
		flex: 1;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 0 11px;
		height: 34px;
		background: var(--bg-input);
		border: 1px solid var(--border);
		border-radius: 7px;
	}

	.search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }

	.search-input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		font-size: 13px;
		color: var(--fg);
		min-width: 0;
	}
	.search-input::placeholder { color: var(--fg-dim); }
	.hb-mono { font-family: var(--font-mono); }

	.result { display: flex; flex-direction: column; gap: 12px; }

	/* Profile card (browse style) */
	.profile-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		overflow: hidden;
	}

	.profile-banner {
		height: 52px;
		background: linear-gradient(135deg, oklch(0.30 0.10 280) 0%, oklch(0.25 0.12 320) 100%);
		border-bottom: 1px solid var(--border);
	}

	.profile-inner {
		padding: 0 16px 16px;
		margin-top: -26px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.profile-top { display: flex; gap: 12px; align-items: flex-end; }

	.profile-name-col { flex: 1; min-width: 0; padding-bottom: 4px; }

	.name-row { display: flex; gap: 8px; align-items: center; margin-bottom: 3px; flex-wrap: wrap; }

	.peer-name { font-weight: 600; font-size: 15px; letter-spacing: -0.2px; }

	.mono { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	.profile-actions { display: flex; gap: 8px; padding-bottom: 4px; }

	.peer-bio { font-size: 13px; color: var(--fg); line-height: 1.55; margin: 0; }

	/* §7 fingerprint row on the lookup card */
	.fp-row { display: flex; align-items: center; gap: 7px; margin-top: 2px; }
	.fp-swatch {
		width: 14px; height: 14px; border-radius: 4px;
		border: 1px solid var(--border-strong); flex-shrink: 0;
	}
	.fp-words { font-size: 11.5px; color: var(--fg-muted); }

	/* Content-type badges + profile tags — the rich public fields a teaser carries */
	.badge-row-sm { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
	.ct-badge {
		font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
		background: var(--bg-elev3); color: var(--fg-muted);
		border: 1px solid var(--border);
	}
	.peer-tags { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 6px; }
	.peer-tag {
		font-size: 10.5px; padding: 2px 8px; border-radius: 999px;
		color: var(--accent);
		background: color-mix(in oklch, var(--accent) 12%, transparent);
	}

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
	/* M15 W1: buttons unified on the app.css .btn system (local copies removed). */

	/* ── §6 Discover hoarders ───────────────────────────────────────────────────────────────── */
	.discover-section {
		border: 1px solid var(--border);
		border-radius: 9px;
		background: var(--bg-elev1);
		overflow: hidden;
	}
	.discover-toggle {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 10px 14px;
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg);
		font-family: var(--font-ui);
	}
	.discover-toggle:hover { background: var(--bg-elev2); }
	.discover-toggle-label { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; font-weight: 600; }
	.discover-chevron { display: flex; color: var(--fg-muted); transition: transform 0.15s; }
	.discover-chevron.open { transform: rotate(180deg); }
	.discover-body { padding: 4px 14px 14px; border-top: 1px solid var(--divider); display: flex; flex-direction: column; gap: 10px; }
	.discover-sub { font-size: 11.5px; color: var(--fg-dim); margin-top: 8px; }
	.ct-row { display: flex; flex-wrap: wrap; gap: 6px; }
	.ct-chip {
		font-size: 11.5px; padding: 4px 11px; border-radius: 999px;
		background: var(--bg-elev2); color: var(--fg-muted);
		border: 1px solid var(--border); cursor: pointer; font-family: var(--font-ui);
		transition: background 0.1s, color 0.1s, border-color 0.1s;
	}
	.ct-chip:hover { background: var(--bg-elev3); }
	.ct-on { background: var(--accent-soft); color: var(--accent); border-color: color-mix(in oklch, var(--accent) 35%, transparent); font-weight: 600; }
	.disc-tag-row { display: flex; gap: 8px; }
	.disc-tag-input {
		flex: 1; background: var(--bg-elev2); border: 1px solid var(--border); border-radius: 7px;
		padding: 7px 10px; font-size: 12.5px; color: var(--fg); font-family: var(--font-ui); outline: none;
	}
	.disc-tag-input::placeholder { color: var(--fg-dim); }
	.disc-tag-input:focus { border-color: var(--accent); }
	.discover-error { font-size: 11.5px; color: oklch(0.75 0.15 25); }
	.discover-results { display: grid; grid-template-columns: repeat(auto-fill, minmax(232px, 1fr)); gap: 12px; }
	.discover-empty { text-align: center; color: var(--fg-dim); font-size: 12.5px; padding: 18px 0; }
	.hit-card {
		display: flex; flex-direction: column; gap: 7px; padding: 13px;
		background: var(--bg-elev2); border: 1px solid var(--border); border-radius: 9px;
	}
	.hit-top { display: flex; align-items: center; gap: 9px; }
	.hit-id { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 1px; }
	.hit-name { font-size: 13px; font-weight: 600; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.hit-stranger { font-size: 9.5px; color: oklch(0.72 0.13 70); }
	.hit-follow {
		padding: 4px 12px; border-radius: 6px; background: var(--accent); color: var(--accent-text);
		border: none; font-size: 11.5px; font-weight: 600; cursor: pointer; font-family: var(--font-ui); flex-shrink: 0;
	}
	.hit-bio { font-size: 11.5px; color: var(--fg-muted); overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
	.hit-fp { display: flex; align-items: center; gap: 6px; font-size: 10px; color: var(--fg-dim); font-family: var(--font-mono); }
	.hit-fp-swatch { width: 10px; height: 10px; border-radius: 3px; flex-shrink: 0; }
	.hit-tags { display: flex; flex-wrap: wrap; gap: 4px; }
	.hit-tag { font-size: 9.5px; padding: 1px 5px; border-radius: 999px; background: var(--bg-elev3); color: var(--fg-muted); border: 1px solid var(--border); }
	.hit-tag-ct { background: var(--accent-soft); color: var(--accent); border-color: color-mix(in oklch, var(--accent) 30%, transparent); }
	.hit-locked { display: inline-flex; align-items: center; font-size: 11px; color: var(--fg-dim); margin-top: 2px; }
</style>
