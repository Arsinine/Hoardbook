<script lang="ts">
	// A-Z jump rail for the Contacts phonebook redesign (devtest #17/#18) — a vertical strip of native
	// buttons, one per `contacts-view.ts::ALPHABET` entry, disabled when that letter's section has no
	// contacts. Clicking scrolls the matching sticky section header into view.
	interface RailTarget {
		label: string;
		anchorId: string;
		enabled: boolean;
	}

	interface Props {
		targets: RailTarget[];
		onjump?: (anchorId: string) => void;
	}

	let { targets, onjump }: Props = $props();

	function jump(anchorId: string) {
		document.getElementById(anchorId)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
		onjump?.(anchorId);
	}
</script>

<nav class="az-rail" aria-label="Jump to section">
	{#each targets as t (t.anchorId)}
		<button
			type="button"
			class="az-rail-btn"
			aria-label={`Jump to ${t.label}`}
			disabled={!t.enabled}
			onclick={() => jump(t.anchorId)}
		>{t.label}</button>
	{/each}
</nav>

<style>
	.az-rail {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 1px;
		padding: 8px 4px;
		flex-shrink: 0;
	}
	.az-rail-btn {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--fg-muted);
		font-size: 9.5px;
		font-weight: 600;
		font-family: var(--font-ui);
		line-height: 1;
		padding: 2px 3px;
		border-radius: 3px;
	}
	.az-rail-btn:hover:not(:disabled) { background: var(--bg-elev2); color: var(--accent); }
	.az-rail-btn:disabled { color: var(--fg-dim); opacity: 0.35; cursor: default; }
</style>
