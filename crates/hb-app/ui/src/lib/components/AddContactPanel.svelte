<script lang="ts">
	// "+ Add contact" modal (devtest #17/#18 phonebook redesign) — the lookup-by-ID card. "Add contact"
	// calls `onadd`, which the page routes into its existing `openAddContact` → AddContactDialog →
	// completeFollow funnel (petname + group picker, then `follow`).
	import { pasteKey } from '../api.js';
	import { contacts, identity, toast } from '../stores.js';
	import { icons, avatarHue } from '../icons.js';
	import Avatar from './Avatar.svelte';
	import FeatureTooltip from './FeatureTooltip.svelte';
	import Modal from './Modal.svelte';
	import type { CachedPeer } from '../types.js';
	import { renderFingerprint } from '../identity-display.js';

	interface Props {
		open?: boolean;
		// `code` is what `follow` must re-resolve — the full `hbk1…` share code (carrying the
		// browse-key). Passing only the npub (as before) silently dropped the key and made every
		// added contact keyless (devtest #3).
		// M20 W2: `resolved` is the CachedPeer the lookup already produced — carried through so the
		// follow leg skips a SECOND resolve.
		onadd?: (code: string, npub: string, displayName: string, resolved: CachedPeer | null) => void;
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
	// devtest #4: a contact added by bare npub is keyless. Pasting their FULL share code later
	// must be allowed to attach the browse-key (re-adding overwrites the stored contact) — otherwise
	// the "Added"/disabled button dead-ends the upgrade and they stay permanently unbrowseable.
	let canUnlock = $derived(!!result?.browse_key_hex && !!existingContact && !existingContact.has_browse_key);

	async function handleLookup() {
		const id = input.trim();
		if (!id) return;
		// devtest #14 self-guard — the same exact-match check the page used to run inline.
		if (id === $identity?.npub) {
			toast("That's your own ID. You can't add yourself as a contact.", 'error');
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
		// M20 W2: carry the lookup's resolved peer through to the follow leg so it isn't resolved twice.
		onadd?.(lookedUpCode, result.npub, result.profile?.display_name ?? '', result);
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') handleLookup();
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
						<div class="hb-input search-input-wrap">
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

	/* QURATOR-101 — on the .hb-input contract; only the icon-prefix layout (gap) is local. The
	   inner input is transparent/borderless so the wrapper reads as one input field. */
	.search-input-wrap {
		flex: 1;
		gap: 8px;
	}

	.search-icon { color: var(--fg-dim); display: flex; flex-shrink: 0; }

	.search-input {
		flex: 1;
		background: transparent;
		border: none;
		outline: none;
		min-width: 0;
	}
	.search-input::placeholder { color: var(--fg-dim); }

	.result { display: flex; flex-direction: column; gap: 12px; }

	/* Profile card (browse style) */
	.profile-card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: var(--r-card, 10px);
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

</style>
