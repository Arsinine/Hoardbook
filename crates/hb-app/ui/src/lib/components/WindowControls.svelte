<script lang="ts">
	// QURATOR-81 — the custom window chrome (minimize / maximize-restore / close), drawn as app
	// chrome on Windows + Linux where the OS title bar was removed (`set_decorations(false)` in
	// lib.rs setup). macOS keeps its native traffic lights, so this component renders nothing there.
	//
	// TRAP 1 (the load-bearing one): the close button calls `getCurrentWindow().close()`, which
	// emits `CloseRequested`. The Rust `on_window_event` handler intercepts that to prevent_close +
	// hide-to-tray — the app's deliberate "close hides, tray Quit exits" design. Wiring close to
	// `.destroy()` or `exit()` here would silently convert hide-to-tray into a quit and no vitest
	// suite would ever catch it. The `q81-close-routes-to-hide.test.ts` source-scan pins this.
	//
	// TRAP 4: the maximize icon tracks state (becomes a restore icon when maximized) via the
	// `isMaximized()` poll on mount + the onResized listener. The host these sit in carries
	// `data-tauri-drag-region`; clicking a button does NOT start a drag because that attribute
	// applies only to the element it is on and does not inherit to children (Tauri v2 docs). The
	// `app-region: no-drag` below is defensive, not the mechanism — do not rely on it. A visible gap
	// separates these from any route action button, so "close the app" never sits flush against
	// "Add contact" etc.
	import { onMount } from 'svelte';
	import { getCurrentWindow } from '@tauri-apps/api/window';

	// macOS keeps its native traffic lights; the custom chrome is Windows + Linux only (TRAP 5).
	const isMac =
		typeof navigator !== 'undefined' && /Mac|iPhone|iPad|iPod/i.test(navigator.platform);

	let maximized = $state(false);

	onMount(() => {
		if (isMac) return;
		// IIFE so onMount gets a sync cleanup function; the async listener setup runs inside.
		let unlisten: (() => void) | undefined;
		(async () => {
			const win = getCurrentWindow();
			try {
				maximized = await win.isMaximized();
			} catch { /* not in Tauri (vitest/jsdom) — stays false */ }
			try {
				unlisten = await win.onResized(async () => {
					try { maximized = await win.isMaximized(); } catch { }
				});
			} catch { }
		})();
		return () => { unlisten?.(); };
	});

	async function minimize() {
		try { await getCurrentWindow().minimize(); } catch { }
	}
	async function toggle_maximize() {
		try { await getCurrentWindow().toggleMaximize(); } catch { }
	}
	// TRAP 1: `.close()` emits CloseRequested → the Rust handler hides-to-tray. NOT destroy()/exit().
	async function close_to_tray() {
		try { await getCurrentWindow().close(); } catch { }
	}
</script>

{#if !isMac}
	<div class="win-controls" aria-label="Window controls">
		<button
			type="button"
			class="win-btn win-min"
			onclick={minimize}
			aria-label="Minimize"
			title="Minimize"
		>
			<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round">
				<line x1="1.5" y1="5" x2="8.5" y2="5" />
			</svg>
		</button>
		<button
			type="button"
			class="win-btn win-max"
			onclick={toggle_maximize}
			aria-label={maximized ? 'Restore' : 'Maximize'}
			title={maximized ? 'Restore' : 'Maximize'}
		>
			{#if maximized}
				<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.1">
					<rect x="1.25" y="3.25" width="5.5" height="5.5" rx="0.75" />
					<path d="M3.25 3.25 V1.75 a0.75 0.75 0 0 1 0.75 -0.75 h4.5 a0.75 0.75 0 0 1 0.75 0.75 v4.5 a0.75 0.75 0 0 1 -0.75 0.75 H7.25" fill="none" />
				</svg>
			{:else}
				<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.1">
					<rect x="1.5" y="1.5" width="7" height="7" rx="0.75" />
				</svg>
			{/if}
		</button>
		<button
			type="button"
			class="win-btn win-close"
			onclick={close_to_tray}
			aria-label="Close"
			title="Close"
		>
			<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round">
				<line x1="2" y1="2" x2="8" y2="8" />
				<line x1="8" y1="2" x2="2" y2="8" />
			</svg>
		</button>
	</div>
{/if}

<style>
	.win-controls {
		display: flex;
		align-items: center;
		gap: 2px;
		flex-shrink: 0;
		/* The drag region is the topbar strip beside these buttons. These buttons must NOT be
		   drag regions themselves, or clicking them starts a window drag instead of the action. */
		app-region: no-drag;
	}

	.win-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 30px;
		height: 28px;
		border-radius: 6px;
		color: var(--fg-muted);
		cursor: pointer;
		transition: background 0.1s, color 0.1s;
	}

	.win-btn:hover { background: var(--bg-elev3); color: var(--fg); }
	.win-btn:active { background: var(--bg-elev2); }

	.win-close:hover {
		background: var(--error);
		color: #fff;
	}

	.win-btn:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 1px;
	}
</style>
