<script lang="ts">
	// M15 W2 — the one modal shell. Owns the backdrop, the accessible dialog card, Escape-to-close,
	// a focus trap, and focus restore on close. Inner form markup + callbacks live in the caller's
	// `children`/`actions` snippets and stay unchanged across migrations.
	import { tick } from 'svelte';
	import { focusableWithin, nextFocus } from '../modal-focus.js';

	interface Props {
		open: boolean;
		onclose: () => void;                 // Escape, backdrop click, or a close control
		title?: string;                      // renders <h2>, wired to aria-labelledby
		level?: 'base' | 'stacked';          // z-scale: --z-modal | --z-modal-stacked
		width?: string;                      // card width (default 460px) — modals vary
		padding?: string;                    // card padding (default 18px); '0' for self-padded bodies
		closeOnBackdrop?: boolean;           // default true
		children: import('svelte').Snippet;
		actions?: import('svelte').Snippet;  // optional footer row
	}

	let { open, onclose, title, level = 'base', width = '460px', padding = '18px', closeOnBackdrop = true, children, actions }: Props = $props();

	let cardEl: HTMLElement | undefined = $state();
	const titleId = 'modal-title-' + Math.random().toString(36).slice(2, 9);
	let zIndex = $derived(level === 'stacked' ? 'var(--z-modal-stacked)' : 'var(--z-modal)');

	// One effect keyed on `open`: capture the opener → focus into the dialog after render → on
	// close/unmount, restore focus to whatever was focused before opening.
	$effect(() => {
		if (!open) return;
		const opener = document.activeElement as HTMLElement | null;
		tick().then(() => {
			if (!cardEl) return;
			const list = focusableWithin(cardEl);
			(list[0] ?? cardEl).focus();
		});
		return () => opener?.focus?.();
	});

	// Keydown is caught on the backdrop via bubbling from the focused descendant — so with stacked
	// modals only the one that actually holds focus reacts to Escape/Tab (no document listener that
	// every modal would fire at once).
	function onKey(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			onclose();
			return;
		}
		if (e.key === 'Tab' && cardEl) {
			// stopPropagation parallels the Escape branch: the dialogs render as route-level siblings
			// today (so nothing cross-propagates), but this keeps the trap correct even if a modal is
			// ever nested inside another's DOM (chorus gemini catch).
			e.stopPropagation();
			const list = focusableWithin(cardEl);
			if (list.length === 0) {
				e.preventDefault();
				return;
			}
			const target = nextFocus(list, document.activeElement, e.shiftKey);
			if (target) {
				e.preventDefault();
				target.focus();
			}
		}
	}

	function onBackdrop(e: MouseEvent) {
		if (closeOnBackdrop && e.target === e.currentTarget) onclose();
	}
</script>

{#if open}
	<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
	<div class="modal-backdrop" style="z-index:{zIndex}" onclick={onBackdrop} onkeydown={onKey} role="presentation">
		<div
			class="modal-card"
			bind:this={cardEl}
			style="width:{width}; padding:{padding}"
			role="dialog"
			aria-modal="true"
			aria-labelledby={title ? titleId : undefined}
			tabindex="-1"
		>
			{#if title}<h2 id={titleId} class="modal-title">{title}</h2>{/if}
			<div class="modal-body">
				{@render children()}
			</div>
			{#if actions}
				<div class="modal-actions">
					{@render actions()}
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.modal-backdrop {
		position: fixed;
		inset: 0;
		background: oklch(0.10 0.005 260 / 0.7);
		backdrop-filter: blur(4px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 30px;
	}
	.modal-card {
		/* width comes from the inline `width` prop (default 460px); this caps it on small viewports */
		max-width: calc(100vw - 40px);
		max-height: calc(100vh - 60px);
		overflow: auto;
		background: var(--bg-elev2);
		border: 1px solid var(--border);
		border-radius: 10px;
		box-shadow: 0 30px 80px -20px oklch(0 0 0 / 0.7), 0 0 0 1px oklch(1 0 0 / 0.06);
		/* padding comes from the inline `padding` prop (default 18px) */
	}
	.modal-card:focus-visible { outline: none; } /* the card is only a focus fallback, not a target */
	.modal-title { font-size: 15px; font-weight: 600; margin-bottom: 12px; color: var(--fg); }
	.modal-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
</style>
