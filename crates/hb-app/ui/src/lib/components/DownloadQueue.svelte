<script lang="ts">
	import { downloads } from '$lib/stores.js';
	import { cancelDownload } from '$lib/api.js';
	import type { DownloadItem } from '$lib/types.js';

	function formatSpeed(bps: number): string {
		if (bps >= 1_000_000) return (bps / 1_000_000).toFixed(1) + ' MB/s';
		if (bps >= 1_000) return (bps / 1_000).toFixed(0) + ' KB/s';
		return bps + ' B/s';
	}

	function formatEta(item: DownloadItem): string {
		if (item.bytes_per_sec === 0 || item.bytes_total === 0) return '—';
		const remaining = item.bytes_total - item.bytes_done;
		const secs = Math.ceil(remaining / item.bytes_per_sec);
		if (secs < 60) return `${secs}s`;
		if (secs < 3600) return `${Math.ceil(secs / 60)}m`;
		return `${(secs / 3600).toFixed(1)}h`;
	}

	function progressPct(item: DownloadItem): number {
		if (item.bytes_total === 0) return 0;
		return Math.min(100, Math.round((item.bytes_done / item.bytes_total) * 100));
	}

	function fmtBytes(b: number): string {
		if (b >= 1e9) return (b / 1e9).toFixed(1) + ' GB';
		if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
		if (b >= 1e3) return (b / 1e3).toFixed(0) + ' KB';
		return b + ' B';
	}

	async function doCancel(id: number) {
		await cancelDownload(id).catch(() => {});
	}

	function dismiss(id: number) {
		downloads.update(list => list.filter(d => d.id !== id));
	}

	$: activeCount = $downloads.filter(d => d.status === 'active').length;
	$: allDone = $downloads.length > 0 && activeCount === 0;
</script>

{#if $downloads.length > 0}
<div class="dq-panel">
	<div class="dq-header">
		<span class="dq-title">
			Downloads
			{#if activeCount > 0}
				<span class="dq-badge">{activeCount}</span>
			{/if}
		</span>
		{#if allDone}
			<button class="dq-clear" on:click={() => downloads.set([])}>Clear all</button>
		{/if}
	</div>

	<div class="dq-list">
		{#each $downloads as item (item.id)}
			<div class="dq-item"
				class:dq-item-done={item.status === 'done'}
				class:dq-item-error={item.status === 'error'}
				class:dq-item-cancelled={item.status === 'cancelled'}
			>
				<div class="dq-row1">
					<span class="dq-name" title={item.filename}>{item.filename}</span>
					{#if item.status === 'active'}
						<button class="dq-x" on:click={() => doCancel(item.id)} title="Cancel download">✕</button>
					{:else}
						<button class="dq-x" on:click={() => dismiss(item.id)} title="Dismiss">✕</button>
					{/if}
				</div>

				{#if item.status === 'active'}
					<div class="dq-bar-track">
						<div class="dq-bar-fill" style="width:{progressPct(item)}%" />
					</div>
					<div class="dq-row2">
						<span class="dq-pct">{progressPct(item)}%</span>
						{#if item.bytes_total > 0}
							<span class="dq-size">{fmtBytes(item.bytes_done)}/{fmtBytes(item.bytes_total)}</span>
						{/if}
						<span class="dq-speed">{formatSpeed(item.bytes_per_sec)}</span>
						<span class="dq-eta">ETA {formatEta(item)}</span>
					</div>
				{:else if item.status === 'done'}
					<div class="dq-bar-track">
						<div class="dq-bar-fill dq-bar-done" style="width:100%" />
					</div>
					<div class="dq-status dq-status-ok">
						Done · {fmtBytes(item.bytes_total)}
					</div>
				{:else if item.status === 'cancelled'}
					<div class="dq-status dq-status-dim">Cancelled</div>
				{:else if item.status === 'error'}
					<div class="dq-status dq-status-err">{item.error ?? 'Error'}</div>
				{/if}
			</div>
		{/each}
	</div>
</div>
{/if}

<style>
	.dq-panel {
		width: 256px;
		flex-shrink: 0;
		border-left: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: var(--bg);
	}

	.dq-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 12px 12px 10px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}

	.dq-title {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.6px;
		text-transform: uppercase;
		color: var(--fg-dim);
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.dq-badge {
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

	.dq-clear {
		font-size: 10.5px;
		color: var(--fg-dim);
		background: transparent;
		border: none;
		cursor: pointer;
		padding: 2px 5px;
		border-radius: 4px;
		font-family: var(--font-ui);
	}

	.dq-clear:hover { background: var(--bg-elev2); color: var(--fg); }

	.dq-list {
		overflow-y: auto;
		flex: 1;
		padding: 8px 0;
	}

	.dq-item {
		padding: 8px 12px;
		border-bottom: 1px solid var(--divider);
	}

	.dq-item:last-child { border-bottom: none; }

	.dq-row1 {
		display: flex;
		align-items: flex-start;
		gap: 6px;
		margin-bottom: 5px;
	}

	.dq-name {
		flex: 1;
		font-size: 11.5px;
		font-weight: 500;
		color: var(--fg);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.dq-item-done .dq-name { color: var(--fg-muted); }
	.dq-item-cancelled .dq-name { color: var(--fg-dim); }
	.dq-item-error .dq-name { color: var(--fg-muted); }

	.dq-x {
		flex-shrink: 0;
		background: transparent;
		border: none;
		cursor: pointer;
		font-size: 10px;
		color: var(--fg-dim);
		padding: 1px 3px;
		border-radius: 3px;
		line-height: 1;
		margin-top: 1px;
	}

	.dq-x:hover { background: var(--bg-elev2); color: var(--fg); }

	.dq-bar-track {
		height: 3px;
		background: var(--bg-elev2);
		border-radius: 2px;
		overflow: hidden;
		margin-bottom: 5px;
	}

	.dq-bar-fill {
		height: 100%;
		background: var(--accent);
		border-radius: 2px;
		transition: width 0.25s ease;
	}

	.dq-bar-done { background: oklch(0.65 0.15 145); }

	.dq-row2 {
		display: flex;
		align-items: center;
		gap: 5px;
		flex-wrap: wrap;
	}

	.dq-pct {
		font-size: 10px;
		font-weight: 600;
		color: var(--accent);
		font-family: var(--font-mono);
		font-feature-settings: 'tnum';
	}

	.dq-size, .dq-speed, .dq-eta {
		font-size: 10px;
		color: var(--fg-dim);
		font-family: var(--font-mono);
		font-feature-settings: 'tnum';
	}

	.dq-size::before { content: '·'; margin-right: 5px; }
	.dq-speed::before { content: '·'; margin-right: 5px; }
	.dq-eta::before { content: '·'; margin-right: 5px; }

	.dq-status {
		font-size: 10.5px;
		margin-top: 2px;
	}

	.dq-status-ok { color: oklch(0.65 0.15 145); }
	.dq-status-dim { color: var(--fg-dim); }
	.dq-status-err { color: oklch(0.65 0.18 25); }
</style>
