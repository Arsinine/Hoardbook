<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import { getIdentity, getProfile, getCollections, getContacts } from '$lib/api.js';
	import { identity, profile, collections, contacts, toastMessage, appReady, unreadCount } from '$lib/stores.js';
	import { navIcons, avatarHue } from '$lib/icons.js';
	import Avatar from '$lib/components/Avatar.svelte';

	onMount(async () => {
		try { identity.set(await getIdentity()); } catch { }
		try { profile.set(await getProfile()); } catch { }
		try { collections.set(await getCollections()); } catch { }
		try { contacts.set(await getContacts()); } catch { }
		appReady.set(true);
	});

	const navItems = [
		{ href: '/', label: 'Home' },
		{ href: '/contacts', label: 'Contacts' },
		{ href: '/chat', label: 'Chat' },
		{ href: '/settings', label: 'Settings' },
	];

	$: currentPath = $page.url.pathname;
	$: idName = $profile?.display_name ?? 'You';
	$: idInitial = idName[0]?.toUpperCase() ?? 'Y';
	$: idShort = $identity ? $identity.hb_id.slice(0, 8) + '…' + $identity.hb_id.slice(-4) : '';
	$: idHue = avatarHue(idInitial);
</script>

<div class="frame">
	<!-- Sidebar -->
	<div class="sidebar">
		<!-- Brand -->
		<div class="brand">
			<div class="brand-logo">H</div>
			<span class="brand-name">Hoardbook</span>
		</div>

		<!-- Nav items -->
		{#each navItems as item}
			{@const active = currentPath === item.href}
			<a href={item.href} class="nav-item" class:nav-active={active}>
				<span class="nav-icon" class:nav-icon-active={active}>{@html navIcons[item.label]}</span>
				{item.label}
				{#if item.label === 'Chat' && $unreadCount > 0}
					<span class="nav-badge">{$unreadCount > 99 ? '99+' : $unreadCount}</span>
				{/if}
			</a>
		{/each}

		<div style="flex:1" />

		<!-- Identity card -->
		{#if $identity}
			<div class="id-card">
				<Avatar letter={idInitial} size={28} hue={idHue} />
				<div class="id-info">
					<div class="id-name">{idName}</div>
					<div class="id-key">{idShort}</div>
				</div>
			</div>
		{/if}
	</div>

	<!-- Main -->
	<div class="main">
		<slot />
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
		background: linear-gradient(135deg, var(--accent) 0%, oklch(0.55 0.18 100) 100%);
		display: flex; align-items: center; justify-content: center;
		font-weight: 800; font-size: 13px;
		color: var(--accent-text);
		letter-spacing: -0.5px;
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
		z-index: 9999;
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
