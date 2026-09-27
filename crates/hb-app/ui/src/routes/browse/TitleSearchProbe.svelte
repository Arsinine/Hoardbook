<script lang="ts">
	// QURATOR-342 D2 — test fixture, vi.mock'd over `$lib/components/TitleSearch.svelte` in
	// q342-browse-teaser.test.ts. The real component (lane D1) self-fetches `searchTitles()` and
	// the orchestrator stub renders NOTHING, so a mount test can neither see the strip nor drive
	// its onbrowse ramp. This stand-in renders one button whose click issues `onbrowse` with the
	// npub the test leaves on `window.__TS_PROBE_NPUB__` — the page's WIRING is asserted through
	// a real component mount (CLAUDE.md §9: mount first), never a source-scan.
	interface Props {
		onbrowse: (npub: string) => void;
	}
	let { onbrowse }: Props = $props();

	function fire(): void {
		const npub = (window as unknown as { __TS_PROBE_NPUB__?: string }).__TS_PROBE_NPUB__ ?? '';
		onbrowse(npub);
	}
</script>

<button data-testid="ts-probe" onclick={fire}>title-search probe</button>
