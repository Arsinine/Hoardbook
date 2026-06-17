<script lang="ts">
	import type { CachedPeer } from '../types.js';
	import Avatar from './Avatar.svelte';
	import { avatarHue } from '$lib/icons.js';

	export let peer: CachedPeer;

	$: name = peer.profile?.display_name ?? 'Unknown';
	$: initial = name[0]?.toUpperCase() ?? '?';
	$: hue = avatarHue(initial);
	$: shortId = peer.npub.length > 14
		? peer.npub.slice(0, 8) + '…' + peer.npub.slice(-4)
		: peer.npub;
</script>

<div class="card">
	<Avatar letter={initial} size={42} {hue} />

	<div class="info">
		<div class="name-row">
			<span class="name">{name}</span>
			{#if peer.online}
				<span class="pill pill-online"><span class="pill-dot" /> Online</span>
			{:else}
				<span class="pill pill-offline">Offline</span>
			{/if}
		</div>

		<div class="mono">{shortId}</div>

		{#if peer.profile?.bio}
			<p class="bio">{peer.profile.bio}</p>
		{/if}

		<div class="metrics">
			{#if peer.profile?.est_size}
				<span class="metric-val">~{peer.profile.est_size}</span>
				<span class="divider" />
			{/if}
			{#if peer.collections.length > 0}
				<span class="metric-val">{peer.collections.length} collection{peer.collections.length !== 1 ? 's' : ''}</span>
			{/if}
		</div>
	</div>

	<slot />
</div>

<style>
	.card {
		background: var(--bg-elev1);
		border: 1px solid var(--border);
		border-radius: 10px;
		padding: 14px;
		display: flex;
		gap: 14px;
		align-items: flex-start;
	}

	.info { flex: 1; min-width: 0; }

	.name-row { display: flex; align-items: center; gap: 8px; margin-bottom: 3px; }

	.name { font-size: 14px; font-weight: 600; letter-spacing: -0.2px; }

	.mono { font-family: var(--font-mono); font-size: 11px; color: var(--fg-muted); }

	.bio { font-size: 12.5px; color: var(--fg-muted); margin-top: 6px; line-height: 1.5; }

	.metrics {
		display: flex;
		gap: 14px;
		margin-top: 8px;
		font-size: 11.5px;
		color: var(--fg-muted);
		align-items: center;
	}

	.metric-val { font-feature-settings: 'tnum'; }

	.divider { width: 1px; height: 12px; background: var(--divider); }

	.pill {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		font-size: 10.5px;
		font-weight: 500;
		padding: 2px 8px;
		border-radius: 999px;
		letter-spacing: 0.2px;
	}

	.pill-dot {
		width: 5px; height: 5px;
		border-radius: 50%;
	}

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
</style>
