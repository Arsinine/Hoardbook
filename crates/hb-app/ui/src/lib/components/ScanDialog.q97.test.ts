// @vitest-environment jsdom
// QURATOR-97 — ScanDialog's close() wipes path / alias / tree, so a stray backdrop click silently
// discards a scan-in-progress. The modal must not close on backdrop; Cancel and Escape stay.
// Behavioural mount: click the real backdrop, assert the dialog and the typed state survive.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/svelte';
import { tick } from 'svelte';
import ScanDialog from './ScanDialog.svelte';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../api.js', () => ({
	scanDirectory: vi.fn(),
	listSubdirs: vi.fn().mockResolvedValue([]),
}));
vi.mock('../stores.js', () => ({ toast: vi.fn() }));
vi.mock('$lib/icons.js', () => ({ icons: { close: '×', folder: '▤' } }));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('QURATOR-97 — ScanDialog ignores backdrop clicks', () => {
	it('a backdrop click neither closes the dialog nor wipes the typed path/alias', async () => {
		const onclose = vi.fn();
		const { container, getByPlaceholderText } = render(ScanDialog, {
			props: { open: true, onclose },
		});

		await fireEvent.input(getByPlaceholderText(/criterion collection/i), { target: { value: 'Movies' } });
		await fireEvent.input(getByPlaceholderText(/c:\\movies/i), { target: { value: '/mnt/movies' } });

		const backdrop = container.querySelector('.modal-backdrop') as HTMLElement;
		expect(backdrop).toBeTruthy();
		await fireEvent.click(backdrop);
		await tick();

		expect(onclose).not.toHaveBeenCalled();
		expect((getByPlaceholderText(/c:\\movies/i) as HTMLInputElement).value).toBe('/mnt/movies');
		expect((getByPlaceholderText(/criterion collection/i) as HTMLInputElement).value).toBe('Movies');
	});

	it('Cancel stays a deliberate close (the affordance the ticket keeps)', async () => {
		const onclose = vi.fn();
		const { getByRole } = render(ScanDialog, { props: { open: true, onclose } });
		await fireEvent.click(getByRole('button', { name: /cancel/i }));
		await tick();
		expect(onclose).toHaveBeenCalledTimes(1);
	});
});
