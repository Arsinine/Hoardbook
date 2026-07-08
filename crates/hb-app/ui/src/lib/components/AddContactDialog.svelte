<script lang="ts">
	// Petname + group dialog shown at contact-add time (M13 W5 Slice 2) — wired into the Contacts
	// lookup follow, Contacts discovery follow, and the Chat request-accept flow. `displayName` seeds
	// the petname (editable); "Skip" adds the contact with the auto-derived petname and no group.
	import type { Group } from '../types.js';

	interface Props {
		open?: boolean;
		/** The peer's own announced display name — seeds the petname suggestion (editable). */
		displayName?: string;
		groups?: Group[];
		onsave?: (detail: { petname: string; group: string | null }) => void;
		onskip?: () => void;
		onnewGroup?: () => void;
		oncancel?: () => void;
	}

	let { open = false, displayName = '', groups = [], onsave, onskip, onnewGroup, oncancel }: Props = $props();

	let petname = $state('');
	let groupName = $state('');

	// Reseed whenever the dialog transitions closed→open, for whichever peer it's being shown for.
	// Not reactive on purpose — a plain transition-edge flag, never read by the template, so it
	// mustn't be part of the effect's own dependency tracking (avoids a self-triggering effect).
	let wasOpen = false;
	$effect(() => {
		if (open && !wasOpen) {
			wasOpen = true;
			petname = displayName;
			groupName = '';
		} else if (!open && wasOpen) {
			wasOpen = false;
		}
	});

	function save() {
		onsave?.({ petname: petname.trim() || displayName, group: groupName || null });
	}

	function skip() {
		onskip?.();
	}
</script>

{#if open}
	<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
	<div class="modal-backdrop" role="dialog" aria-modal="true" aria-label="Add contact" tabindex="-1" onclick={(e) => { if (e.target === e.currentTarget) oncancel?.(); }}>
		<div class="modal">
			<h2>Add contact</h2>
			<div class="field">
				<label for="acd-petname">Petname</label>
				<input id="acd-petname" type="text" bind:value={petname} placeholder="A nickname only you see" onkeydown={(e) => e.key === 'Enter' && save()} />
			</div>
			<div class="group-row">
				<span class="group-label">Add to group:</span>
				<select bind:value={groupName}>
					<option value="">Ungrouped</option>
					{#each groups as g (g.name)}
						<option value={g.name}>{g.name}</option>
					{/each}
				</select>
				<button type="button" class="link" onclick={() => onnewGroup?.()}>+ New group</button>
			</div>
			<div class="modal-actions">
				<button type="button" class="ghost" onclick={skip}>Skip</button>
				<button type="button" class="btn-primary" onclick={save}>Add contact</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.modal-backdrop {
		position: fixed; inset: 0; z-index: 9998;
		background: oklch(0 0 0 / 0.45);
		display: flex; align-items: center; justify-content: center;
	}
	.modal {
		background: var(--bg-elev1);
		border: 1px solid var(--border-strong);
		border-radius: 12px;
		padding: 18px;
		width: min(380px, 90vw);
		display: flex; flex-direction: column; gap: 12px;
	}
	.modal h2 { font-size: 14px; font-weight: 700; margin: 0; }
	.field { display: flex; flex-direction: column; gap: 5px; }
	.field label { font-size: 11px; color: var(--fg-muted); font-weight: 500; }
	.field input[type='text'] {
		padding: 6px 9px; background: var(--bg-elev2); color: var(--fg);
		border: 1px solid var(--border); border-radius: 6px; font: inherit;
	}
	.group-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
	.group-label { font-size: 11.5px; color: var(--fg-muted); white-space: nowrap; }
	.group-row select {
		flex: 1; min-width: 100px;
		padding: 6px 9px; background: var(--bg-elev2); color: var(--fg);
		border: 1px solid var(--border); border-radius: 6px; font: inherit;
	}
	.link {
		background: transparent; border: none; cursor: pointer; color: var(--accent);
		font: inherit; font-size: 11.5px; padding: 0; white-space: nowrap;
	}
	.link:hover { text-decoration: underline; }
	.modal-actions { display: flex; justify-content: flex-end; gap: 8px; }
	.btn-primary {
		padding: 6px 14px; border-radius: 6px; border: 1px solid var(--accent);
		background: var(--accent); color: var(--accent-text); font: inherit; font-weight: 600; cursor: pointer;
	}
	.ghost {
		padding: 6px 12px; border-radius: 6px; border: 1px solid var(--border);
		background: transparent; color: var(--fg); font: inherit; cursor: pointer;
	}
</style>
