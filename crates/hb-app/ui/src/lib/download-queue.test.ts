import { describe, expect, it } from 'vitest';

// ── Mirrors the DownloadItem shape used in the downloads store ────────────────
interface DownloadItem {
	id: number;
	filename: string;
	save_path: string;
	bytes_done: number;
	bytes_total: number;
	bytes_per_sec: number;
	status: 'active' | 'done' | 'error' | 'cancelled';
	error?: string;
	/** Unix timestamp (ms) when the download started — used to compute ETA. */
	started_at: number;
}

// ── Pure helpers (mirrors what the DownloadQueue component uses) ──────────────

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

// ── Reducer that the downloads store applies when an event arrives ─────────────

interface ProgressEvent {
	id: number;
	filename: string;
	bytes_done: number;
	bytes_total: number;
	bytes_per_sec: number;
	status: DownloadItem['status'];
	error?: string;
}

function applyProgressEvent(items: DownloadItem[], ev: ProgressEvent): DownloadItem[] {
	const idx = items.findIndex(d => d.id === ev.id);
	const patch: DownloadItem = {
		id: ev.id,
		filename: ev.filename,
		save_path: idx >= 0 ? items[idx].save_path : '',
		bytes_done: ev.bytes_done,
		bytes_total: ev.bytes_total,
		bytes_per_sec: ev.bytes_per_sec,
		status: ev.status,
		error: ev.error,
		started_at: idx >= 0 ? items[idx].started_at : Date.now(),
	};
	if (idx >= 0) {
		const next = [...items];
		next[idx] = patch;
		return next;
	}
	return [...items, patch];
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe('download queue helpers', () => {
	it('formatSpeed renders correct units', () => {
		expect(formatSpeed(500)).toBe('500 B/s');
		expect(formatSpeed(1_500)).toBe('2 KB/s');
		expect(formatSpeed(2_500_000)).toBe('2.5 MB/s');
	});

	it('formatEta returns — when speed is 0', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 0,
			bytes_total: 1000, bytes_per_sec: 0, status: 'active', started_at: Date.now() };
		expect(formatEta(item)).toBe('—');
	});

	it('formatEta returns seconds', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 0,
			bytes_total: 1000, bytes_per_sec: 100, status: 'active', started_at: Date.now() };
		expect(formatEta(item)).toBe('10s');
	});

	it('formatEta returns minutes', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 0,
			bytes_total: 600_000, bytes_per_sec: 1_000, status: 'active', started_at: Date.now() };
		expect(formatEta(item)).toBe('10m');
	});

	it('progressPct clamps to 100', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 1100,
			bytes_total: 1000, bytes_per_sec: 0, status: 'active', started_at: Date.now() };
		expect(progressPct(item)).toBe(100);
	});

	it('progressPct is 50 at half way', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 500,
			bytes_total: 1000, bytes_per_sec: 0, status: 'active', started_at: Date.now() };
		expect(progressPct(item)).toBe(50);
	});

	it('progressPct returns 0 when bytes_total is 0', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 0,
			bytes_total: 0, bytes_per_sec: 0, status: 'active', started_at: Date.now() };
		expect(progressPct(item)).toBe(0);
	});
});

describe('download queue store reducer', () => {
	it('adds new download on first event', () => {
		const items: DownloadItem[] = [];
		const ev: ProgressEvent = { id: 1, filename: 'Akira.mkv', bytes_done: 0,
			bytes_total: 1_000_000, bytes_per_sec: 0, status: 'active' };
		const next = applyProgressEvent(items, ev);
		expect(next).toHaveLength(1);
		expect(next[0].filename).toBe('Akira.mkv');
	});

	it('updates existing download in-place', () => {
		const item: DownloadItem = { id: 1, filename: 'Akira.mkv', save_path: '/tmp/Akira.mkv',
			bytes_done: 0, bytes_total: 1_000_000, bytes_per_sec: 0, status: 'active',
			started_at: 1000 };
		const ev: ProgressEvent = { id: 1, filename: 'Akira.mkv', bytes_done: 500_000,
			bytes_total: 1_000_000, bytes_per_sec: 50_000, status: 'active' };
		const next = applyProgressEvent([item], ev);
		expect(next).toHaveLength(1);
		expect(next[0].bytes_done).toBe(500_000);
		expect(next[0].save_path).toBe('/tmp/Akira.mkv'); // preserved
		expect(next[0].started_at).toBe(1000); // preserved
	});

	it('marks download as done', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 900_000,
			bytes_total: 1_000_000, bytes_per_sec: 100_000, status: 'active', started_at: 0 };
		const ev: ProgressEvent = { id: 1, filename: 'f', bytes_done: 1_000_000,
			bytes_total: 1_000_000, bytes_per_sec: 0, status: 'done' };
		const next = applyProgressEvent([item], ev);
		expect(next[0].status).toBe('done');
	});

	it('marks download as cancelled', () => {
		const item: DownloadItem = { id: 1, filename: 'f', save_path: '', bytes_done: 500_000,
			bytes_total: 1_000_000, bytes_per_sec: 50_000, status: 'active', started_at: 0 };
		const ev: ProgressEvent = { id: 1, filename: 'f', bytes_done: 500_000,
			bytes_total: 1_000_000, bytes_per_sec: 0, status: 'cancelled' };
		const next = applyProgressEvent([item], ev);
		expect(next[0].status).toBe('cancelled');
	});

	it('multiple concurrent downloads tracked independently', () => {
		let items: DownloadItem[] = [];
		items = applyProgressEvent(items, { id: 1, filename: 'a.mkv', bytes_done: 0,
			bytes_total: 1000, bytes_per_sec: 0, status: 'active' });
		items = applyProgressEvent(items, { id: 2, filename: 'b.zip', bytes_done: 0,
			bytes_total: 2000, bytes_per_sec: 0, status: 'active' });
		items = applyProgressEvent(items, { id: 1, filename: 'a.mkv', bytes_done: 500,
			bytes_total: 1000, bytes_per_sec: 100, status: 'active' });

		expect(items).toHaveLength(2);
		expect(items.find(d => d.id === 1)?.bytes_done).toBe(500);
		expect(items.find(d => d.id === 2)?.bytes_done).toBe(0);
	});
});
