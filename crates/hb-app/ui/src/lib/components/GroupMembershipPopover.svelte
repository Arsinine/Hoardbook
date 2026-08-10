<script lang="ts">
	// M22 W8 — ONE shared group-membership editor used by BOTH route pages.
	//
	// Extracted from Contacts' W5b inline popover (the `+` beside the group chips) so Browse gains
	// the SAME editor as its keyboard route for add/move/ungrouped. Contacts and Browse are the
	// standing hardened-path / unhardened-sibling drift pair (CLAUDE.md §9) — two copies of this
	// markup would diverge the way the W4 import drift did. This component IS the single copy.
	//
	// Semantics (identical to the old inline popover):
	//   - The draft is seeded from CURRENT memberships when it opens, so the pre-check never loses an
	//     existing membership (the data-loss guard; see contacts-w5-dataloss).
	//   - Apply sends the WHOLE checked set through onapply — the same full-set replace that
	//     `contactUpdateGroups` performs (the shared drag-group + api convergence point).
	//   - Cancel discards; Escape closes; the backdrop closes.
	//   - "+ New group…" is routed to the CALLER's existing create dialog via onnewgroup (each page
	//     owns its own CreateGroupDialog instance).
	//   - This component NEVER calls the audience store. The only write path is the onapply callback;
	//     the caller routes it to contactUpdateGroups (pinned by the audience-independence invariant).
	//
	// Keyboard operability (M22 W8 requirement 5):
	//   - When it opens, focus moves INTO the popover (the first group checkbox, or the Apply button
	//     when there are no groups).
	//   - Enter on a checkbox toggles it (native), Enter on Apply commits, Enter on Cancel/Escape
	//     closes without committing, Tab moves within the popover (native roving).
	//   - When it closes (any path), focus RETURNS to the invoking row. The caller passes
	//     returnFocusTo, a callback that resolves the row DOM node (rowId/ROW_ID_PREFIX lives on the
	//     caller — each list has its own prefix, and the focused row may have been filtered away, so
	//     the caller decides what to focus instead).
	import OverflowMenu from './OverflowMenu.svelte';
	import type { Group } from '$lib/types.js';
	import { tick } from 'svelte';

	interface Props {
		open: boolean;
		anchor?: HTMLElement;
		contactName: string;
		groups: Group[]; // ALL groups the page knows (each renders as a checkbox)
		memberships: string[]; // the contact's CURRENT group memberships
		onapply: (selected: string[]) => void; // full-set replace (contactUpdateGroups)
		onclose: () => void; // cancel / backdrop / Escape
		onnewgroup: () => void; // "+ New group…" routed to the caller's CreateGroupDialog
		returnFocusTo?: () => HTMLElement | undefined | null; // where focus returns on close
	}

	let { open, anchor, contactName, groups, memberships, onapply, onclose, onnewgroup, returnFocusTo }: Props = $props();

	// The working checkbox state. Seeded from `memberships` each time the popover opens so the
	// pre-check never loses an existing membership. Stored as a plain Set (Svelte 5: reassign the
	// whole variable on mutation so the rune sees the change).
	let draft = $state<Set<string>>(new Set());

	// The popover's content root, bound so we can move focus INTO it on open.
	let panelEl: HTMLDivElement | undefined = $state();
	// One-shot latch: set when the popover opens (or is committed/closed); consumed by the close
	// effect so focus is returned to the invoking row exactly once per close.
	let restoreFocus = $state(false);

	function toggle(name: string, checked: boolean) {
		const next = new Set(draft);
		if (checked) next.add(name);
		else next.delete(name);
		draft = next;
	}

	function commit() {
		onapply([...draft]);
		restoreFocus = true;
	}

	function close() {
		onclose();
		restoreFocus = true;
	}

	async function focusFirst() {
		// tick(): the panel does not exist until OverflowMenu's {#if open} renders it.
		await tick();
		const panel = panelEl;
		if (!panel) return;
		const box = panel.querySelector<HTMLInputElement>('input[type="checkbox"]');
		if (box) box.focus();
		else {
			const apply = panel.querySelector<HTMLButtonElement>('.gmp-apply');
			if (apply) apply.focus();
			else panel.focus();
		}
	}

	$effect(() => {
		if (open) {
			draft = new Set(memberships);
			restoreFocus = true;
			void focusFirst();
		}
	});

	// Return focus to the invoking row when the popover closes. A2/A9: .focus() on a detached node
	// is a silent no-op, so the caller's returnFocusTo is responsible for the isConnected check and
	// any fallback to the first rendered row.
	$effect(() => {
		if (!open && restoreFocus) {
			restoreFocus = false;
			const target = returnFocusTo?.();
			if (target) target.focus();
		}
	});
</script>

<OverflowMenu {open} {anchor} onclose={close} minWidth="240px">
	<div class="gmp-content" bind:this={panelEl} aria-label={`${contactName}'s groups`}>
		<div class="gmp-title">{contactName}'s groups</div>
		<div class="gmp-list">
			{#each groups as g (g.name)}
				<label class="gmp-row">
					<input
						type="checkbox"
						checked={draft.has(g.name)}
						onchange={(e) => toggle(g.name, e.currentTarget.checked)}
					/>
					{#if g.color}<span class="gmp-dot" style={`background:${g.color}`}></span>{/if}
					{g.name}
				</label>
			{/each}
			{#if groups.length === 0}
				<span class="gmp-empty">No groups yet.</span>
			{/if}
			<button type="button" class="gmp-new" onclick={onnewgroup}>+ New group…</button>
		</div>
		<div class="gmp-footer">
			<button type="button" class="btn-ghost btn-xs" onclick={close}>Cancel</button>
			<button type="button" class="btn-primary btn-xs gmp-apply" onclick={commit}>Apply</button>
		</div>
	</div>
</OverflowMenu>

<style>
	.gmp-title { font-size: 11px; font-weight: 600; color: var(--fg-muted); padding: 4px 8px 6px; }
	.gmp-list { display: flex; flex-direction: column; gap: 1px; }
	.gmp-row {
		display: inline-flex; align-items: center; gap: 6px;
		font-size: 12px; color: var(--fg); cursor: pointer; padding: 4px 8px; border-radius: 5px;
	}
	.gmp-row:hover { background: var(--bg-elev3); }
	.gmp-row input { margin: 0; cursor: pointer; }
	.gmp-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; display: inline-block; }
	.gmp-empty { font-size: 11px; color: var(--fg-dim); padding: 4px 8px; }
	.gmp-new {
		text-align: left; background: transparent; border: none; cursor: pointer;
		font-family: var(--font-ui); font-size: 11.5px; color: var(--fg-dim);
		padding: 5px 8px; border-radius: 5px; margin-top: 2px;
	}
	.gmp-new:hover { background: var(--bg-elev3); color: var(--fg-muted); }
	.gmp-footer { display: flex; justify-content: flex-end; gap: 6px; padding: 6px 8px 2px; border-top: 1px solid var(--divider); margin-top: 4px; }
</style>
