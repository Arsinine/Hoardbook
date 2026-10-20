<script lang="ts">
	// QURATOR-102 — the shared empty state. Four dialects existed across the routes (Home's `.empty`
	// line, Contacts' `.empty`, Chat's `.convo-empty`/`.thread-empty`, Browse's `.empty-state` card,
	// Topics' `.empty`), differing in size (12–13px), tone and structure. One component now owns the
	// shape; each site passes its message text plus optional affordances:
	//
	//   - `icon`    — an SVG string from lib/icons.ts (never emoji), rendered dim + scaled like
	//                 Browse's empty-icon.
	//   - `cta`     — { label, href } renders a link (e.g. Chat's dead-end pane → /contacts).
	//   - `actions` — QURATOR-390 zero-state actions: { label, onclick?, href? }[] rendered as real
	//                 <button>s — first one btn-primary, the rest btn-default — or <a>s when href is
	//                 set (owner devtest 2026-10-20, "Noun + action button" zero-states).
	//   - `error`   — true renders the message on the --error ramp with a working Retry button.
	//                 This is the QURATOR-93 surface: a FAILED load must never read as a confident
	//                 negative. `onretry` re-runs the fetch; the caller clears the flag on success
	//                 (and a later success clears a stale error — the QURATOR-80/85 rule, both ways).
	//
	// The variant prop is a plain boolean rather than a discriminated union so call sites stay
	// terse: `<EmptyState error message="…" onretry={load} />` vs `<EmptyState message="…" />`.
	interface Props {
		message: string;
		icon?: string;
		cta?: { label: string; href: string };
		/** QURATOR-390 zero-state actions: real buttons (first btn-primary, rest btn-default); href renders an anchor. */
		actions?: { label: string; onclick?: () => void; href?: string }[];
		/** Error variant (QURATOR-93): distinct colour + a Retry affordance. */
		error?: boolean;
		onretry?: () => void;
		/** Centered card (fills its pane) vs inline line (inside a list). Defaults to inline. */
		centered?: boolean;
	}

	let { message, icon, cta, actions, error = false, onretry, centered = false }: Props = $props();
</script>

<div class="hb-empty" class:hb-empty-centered={centered} class:hb-empty-error={error} role={error ? 'alert' : undefined}>
	{#if icon}
		<div class="hb-empty-icon">{@html icon}</div>
	{/if}
	<div class="hb-empty-label">{message}</div>
	{#if actions && actions.length > 0}
		<div class="hb-empty-actions">
			{#each actions as a, i (a.label)}
				{#if a.href}
					<a class={i === 0 ? 'btn-primary btn-sm' : 'btn-default btn-sm'} href={a.href}>{a.label}</a>
				{:else}
					<button type="button" class={i === 0 ? 'btn-primary btn-sm' : 'btn-default btn-sm'} onclick={a.onclick}>{a.label}</button>
				{/if}
			{/each}
		</div>
	{/if}
	{#if error && onretry}
		<button type="button" class="btn-default btn-sm" onclick={onretry}>Retry</button>
	{/if}
	{#if cta}
		<a class="hb-empty-cta" href={cta.href}>{cta.label}</a>
	{/if}
</div>

<style>
	.hb-empty {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 8px;
		padding: 16px 0;
		color: var(--fg-dim);
		font-size: 12.5px;
		max-width: 360px;
	}

	.hb-empty-centered {
		flex: 1;
		align-items: center;
		justify-content: center;
		text-align: center;
		padding: 32px;
	}

	/* QURATOR-93: the failed-load surface is on the --error ramp (same token as .btn-danger and
	   Topics' .root-error), so an unknown reads visually distinct from the muted genuine-empty —
	   never as a confident negative. */
	.hb-empty-error { color: var(--error); }

	.hb-empty-icon {
		opacity: 0.3;
		transform: scale(2.8);
		margin-bottom: 8px;
		display: flex;
	}

	.hb-empty-label { line-height: 1.45; }

	/* QURATOR-390: the action row rides the shared .btn system — no local button styling. */
	.hb-empty-actions {
		display: flex;
		gap: 8px;
		margin-top: 4px;
		flex-wrap: wrap;
	}

	.hb-empty-cta {
		font-size: 12px;
		color: var(--accent);
		text-decoration: none;
		margin-top: 4px;
	}
	.hb-empty-cta:hover { text-decoration: underline; }

	/* The Retry button sits on the shared app.css .btn system — no local button styling to drift. */
</style>
