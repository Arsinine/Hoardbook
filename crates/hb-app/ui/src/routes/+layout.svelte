<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { page } from '$app/stores';
	import { getIdentity, getProfile, getCollections, getContacts, getMessages, getReadState } from '$lib/api.js';
	import { identity, profile, collections, contacts, inboxMessages, readWatermarks, toastMessage, appReady, toast, identityLoadError } from '$lib/stores.js';
	import { totalUnread, unreadByPeer } from '$lib/unread-view.js';
	import { listen } from '@tauri-apps/api/event';
	import { navIcons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';
	import { getVersion } from '@tauri-apps/api/app';
	import { NAV_POLL_VISIBLE_MS } from '$lib/poll-lifecycle.js';
	interface Props {
		children?: import('svelte').Snippet;
	}

	let { children }: Props = $props();

	let appVersion = $state('');

	onMount(() => {
		// Async init (IIFE so onMount can return a sync cleanup function).
		(async () => {
			try {
				identity.set(await getIdentity());
				identityLoadError.set(null);
			} catch (e) {
				// A DPAPI or I/O failure loading the saved keypair. Record the error so
				// the home page can show a recovery screen instead of the onboarding wizard.
				console.error('getIdentity failed:', e);
				identityLoadError.set(String(e));
			}
			try { profile.set(await getProfile()); } catch { }
			try { collections.set(await getCollections()); } catch { }
			try { contacts.set(await getContacts()); } catch { }
			try { appVersion = await getVersion(); } catch { appVersion = '0.4.2'; }

			// Load the persisted per-peer read watermark BEFORE seeding the inbox (devtest #16) — the
			// nav badge derives from both together, so this order avoids a first-paint flash of a
			// stale "everything unread" count.
			try { readWatermarks.set(await getReadState()); } catch { }
			try { inboxMessages.set(await getMessages()); } catch { }

			appReady.set(true);
		})();

		// Update-available event from the backend background check.
		let unlistenUpdate: (() => void) | undefined;
		listen<string>('update-available', (event) => {
			toast(`Update v${event.payload} available — check Settings to install`, 'success');
		}).then(fn => { unlistenUpdate = fn; });

		// Direct DM received via iroh — refresh the inbox; the nav badge (derived from
		// readWatermarks) picks up the new message on its own (devtest #16).
		let unlistenDm: (() => void) | undefined;
		listen<number>('dm-received', () => {
			getMessages().then((msgs) => inboxMessages.set(msgs)).catch(() => { });
		}).then(fn => { unlistenDm = fn; });

		// Background poll: keeps inboxMessages fresh. M12 W1 Decision B: skip the relay read while
		// the window is hidden (tray/minimized) — no reconnect storm against a window nobody is
		// looking at; it resumes automatically when shown. The nav badge re-derives itself from the
		// store on every inboxMessages update — no separate counting here (devtest #16).
		const poll = setInterval(async () => {
			if (!get(identity) || document.hidden) return;
			try {
				const msgs = await getMessages();
				inboxMessages.set(msgs);
			} catch { }
		}, NAV_POLL_VISIBLE_MS);

		// devtest #2: suppress the default webview right-click menu (Reload / Inspect Element etc.) —
		// this is a desktop app, not a web page. The app's own custom menus (e.g. the Browse file
		// row menu) call preventDefault + draw their own UI, so they keep working; this only kills the
		// native fallback. Paste still works via Ctrl+V.
		const suppressContextMenu = (e: MouseEvent) => e.preventDefault();
		document.addEventListener('contextmenu', suppressContextMenu);

		return () => {
			clearInterval(poll);
			unlistenUpdate?.();
			unlistenDm?.();
			document.removeEventListener('contextmenu', suppressContextMenu);
		};
	});

	const navItems = [
		{ href: '/', label: 'Home' },
		{ href: '/contacts', label: 'Contacts' },
		{ href: '/browse', label: 'Browse' },
		{ href: '/topics', label: 'Topics' },
		{ href: '/chat', label: 'Chat' },
		{ href: '/settings', label: 'Settings' },
	];

	let currentPath = $derived($page.url.pathname);
	let idName = $derived($profile?.display_name ?? 'You');
	let idInitial = $derived(idName[0]?.toUpperCase() ?? 'Y');
	let idShort = $derived($identity ? $identity.npub.slice(0, 8) + '…' + $identity.npub.slice(-4) : '');
	let idHue = $derived(avatarHue(idInitial));
	// devtest #16: the nav badge derives straight from the persisted per-peer watermark — it clears
	// per-conversation as each is opened in Chat, not merely by landing on the /chat route.
	let navUnreadCount = $derived(totalUnread(unreadByPeer($inboxMessages, $readWatermarks, $identity?.npub ?? '')));
</script>

<div class="frame">
	<!-- Sidebar -->
	<div class="sidebar">
		<!-- Brand -->
		<div class="brand">
			<div class="brand-logo">
				<svg viewBox="0 0 18 24" width="15" height="20" style="overflow:visible" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
					<line x1="4" y1="-8" x2="4" y2="22"/>
					<path d="M4 12.5 C4 8 15 8 15 12.5 L15 22"/>
				</svg>
			</div>
			<span class="brand-name">Hoardbook</span>
		</div>

		<!-- Nav items -->
		{#each navItems as item}
			{@const active = currentPath === item.href}
			<a href={item.href} class="nav-item" class:nav-active={active}>
				<span class="nav-icon" class:nav-icon-active={active}>{@html navIcons[item.label]}</span>
				{item.label}
				{#if item.label === 'Chat' && navUnreadCount > 0}
					<span class="nav-badge">{navUnreadCount > 99 ? '99+' : navUnreadCount}</span>
				{/if}
			</a>
		{/each}

		<div style="flex:1"></div>

		<!-- Identity card -->
		{#if $identity}
			<div class="id-card">
				<Avatar letter={idInitial} size={28} hue={idHue} picture={$profile?.picture} />
				<div class="id-info">
					<div class="id-name">{idName}</div>
					<div class="id-key">{idShort}</div>
				</div>
			</div>
		{/if}

		{#if appVersion}
			<div class="version-tag">v{appVersion}</div>
		{/if}
	</div>

	<!-- Main -->
	<div class="main">
		{@render children?.()}
	</div>
</div>

<!-- Toast -->
{#if $toastMessage}
	<div class="toast" class:toast-error={$toastMessage.kind === 'error'}>
		{$toastMessage.text}
	</div>
{/if}

<style>
	.frame {
		display: flex;
		width: 100vw;
		height: 100vh;
		background: var(--bg);
		font-family: var(--font-ui);
		color: var(--fg);
		font-size: 13px;
		overflow: hidden;
	}

	.sidebar {
		width: 192px;
		flex-shrink: 0;
		background: var(--bg);
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		padding: 18px 12px;
		gap: 2px;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 0 8px 18px;
		border-bottom: 1px solid var(--divider);
		margin-bottom: 12px;
	}

	.brand-logo {
		width: 24px; height: 24px;
		border-radius: 6px;
		background: var(--bg-elev3);
		border: 1px solid color-mix(in oklch, var(--accent) 22%, transparent);
		display: flex; align-items: center; justify-content: center;
		color: var(--accent);
		overflow: hidden;
	}

	.brand-name {
		font-weight: 700;
		font-size: 14px;
		letter-spacing: -0.3px;
		color: var(--fg);
	}

	.nav-item {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 8px 10px;
		font-size: 13px;
		font-weight: 500;
		color: var(--fg-muted);
		background: transparent;
		border-radius: 7px;
		text-decoration: none;
		cursor: pointer;
		transition: background 0.1s, color 0.1s;
	}

	.nav-active {
		font-weight: 600;
		color: var(--fg);
		background: var(--bg-elev2);
	}

	.nav-icon {
		color: var(--fg-muted);
		display: flex;
		flex-shrink: 0;
	}

	.nav-badge {
		margin-left: auto;
		font-size: 9.5px;
		font-weight: 700;
		padding: 1px 5px;
		border-radius: 999px;
		background: var(--accent);
		color: var(--accent-text);
		min-width: 16px;
		text-align: center;
		font-feature-settings: 'tnum';
	}

	.nav-icon-active {
		color: var(--accent);
	}

	.id-card {
		padding: 10px;
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 8px;
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.id-info {
		min-width: 0;
		flex: 1;
	}

	.id-name {
		font-size: 12px;
		font-weight: 600;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.id-key {
		font-family: var(--font-mono);
		font-size: 9.5px;
		color: var(--fg-dim);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.version-tag {
		font-size: 10px;
		color: var(--fg-dim);
		text-align: center;
		padding: 4px 0 2px;
		font-family: var(--font-mono);
		letter-spacing: 0.3px;
	}

	.main {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

	.toast {
		position: fixed;
		bottom: 16px;
		right: 16px;
		z-index: var(--z-toast);
		padding: 8px 14px;
		border-radius: 8px;
		font-size: 12.5px;
		font-weight: 500;
		background: var(--bg-elev3);
		color: var(--fg);
		border: 1px solid var(--border-strong);
		box-shadow: 0 8px 24px oklch(0 0 0 / 0.4);
	}

	.toast-error {
		background: oklch(0.25 0.06 25);
		border-color: oklch(0.65 0.18 25 / 0.4);
		color: oklch(0.85 0.12 25);
	}
</style>
