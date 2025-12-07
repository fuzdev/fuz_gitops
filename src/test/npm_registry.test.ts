import {describe, it, expect, vi, beforeEach, afterEach} from 'vitest';
import {spawn_out} from '@fuzdev/fuz_util/process.js';
import {wait} from '@fuzdev/fuz_util/async.js';

import {
	check_package_available,
	wait_for_package,
	get_package_info,
	package_exists,
	type WaitOptions,
} from '$lib/npm_registry.js';
import {create_mock_logger} from './test_helpers.ts';

/* eslint-disable @typescript-eslint/require-await */

// Mock spawn_out from @fuzdev/fuz_util/process.js
vi.mock('@fuzdev/fuz_util/process.js', () => ({
	spawn_out: vi.fn(),
}));

// Mock wait from @fuzdev/fuz_util/async.js
vi.mock('@fuzdev/fuz_util/async.js', () => ({
	wait: vi.fn(async () => {
		// Mock implementation
	}),
}));

describe('npm_registry', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	afterEach(() => {
		vi.restoreAllMocks();
	});

	describe('check_package_available', () => {
		it('returns true when package version exists', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.2.3'} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			expect(result).toBe(true);
			expect(spawn_out).toHaveBeenCalledWith('npm', ['view', 'test-pkg@1.2.3', 'version']);
		});

		it('returns false when version does not match', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.2.4'} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			expect(result).toBe(false);
		});

		it('returns false when npm command fails', async () => {
			vi.mocked(spawn_out).mockRejectedValue(new Error('npm error'));

			const result = await check_package_available('test-pkg', '1.2.3');

			expect(result).toBe(false);
		});

		it('returns false when stdout is empty', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			expect(result).toBe(false);
		});

		it('returns false when stdout is undefined', async () => {
			vi.mocked(spawn_out).mockResolvedValue({} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			expect(result).toBe(false);
		});

		it('trims whitespace from stdout', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '  1.2.3\n  '} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			expect(result).toBe(true);
		});

		it('logs debug message on error', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockRejectedValue(new Error('network timeout'));

			await check_package_available('test-pkg', '1.2.3', {log});

			expect(log.debug_calls.length).toBe(1);
			expect(log.debug_calls[0]).toContain('test-pkg@1.2.3');
			expect(log.debug_calls[0]).toContain('network timeout');
		});

		it('handles scoped package names', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '2.0.0'} as any);

			await check_package_available('@scope/package', '2.0.0');

			expect(spawn_out).toHaveBeenCalledWith('npm', ['view', '@scope/package@2.0.0', 'version']);
		});

		it('handles prerelease versions', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0-beta.1'} as any);

			const result = await check_package_available('test-pkg', '1.0.0-beta.1');

			expect(result).toBe(true);
		});

		it('handles build metadata in versions', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0+build.123'} as any);

			const result = await check_package_available('test-pkg', '1.0.0+build.123');

			expect(result).toBe(true);
		});
	});

	describe('wait_for_package', () => {
		it('returns immediately when package is available', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package('test-pkg', '1.0.0');

			expect(spawn_out).toHaveBeenCalledTimes(1);
			expect(wait).not.toHaveBeenCalled();
		});

		it('retries until package becomes available', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 3) {
					return {stdout: ''} as any;
				}
				return {stdout: '1.0.0'} as any;
			});

			await wait_for_package('test-pkg', '1.0.0');

			expect(spawn_out).toHaveBeenCalledTimes(3);
			expect(wait).toHaveBeenCalledTimes(2);
		});

		it('applies exponential backoff', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 4) {
					return {stdout: ''} as any;
				}
				return {stdout: '1.0.0'} as any;
			});

			const options: WaitOptions = {
				initial_delay: 100,
				max_delay: 1000,
			};

			await wait_for_package('test-pkg', '1.0.0', options);

			const wait_calls = vi.mocked(wait).mock.calls;
			expect(wait_calls.length).toBe(3);

			// First delay: ~100ms (+ jitter)
			expect(wait_calls[0]![0]).toBeGreaterThanOrEqual(100);
			expect(wait_calls[0]![0]).toBeLessThan(120);

			// Second delay: ~150ms (100 * 1.5 + jitter)
			expect(wait_calls[1]![0]).toBeGreaterThanOrEqual(150);
			expect(wait_calls[1]![0]).toBeLessThan(180);

			// Third delay: ~225ms (150 * 1.5 + jitter)
			expect(wait_calls[2]![0]).toBeGreaterThanOrEqual(225);
			expect(wait_calls[2]![0]).toBeLessThan(270);
		});

		it('respects max_delay cap', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 10) {
					return {stdout: ''} as any;
				}
				return {stdout: '1.0.0'} as any;
			});

			const options: WaitOptions = {
				initial_delay: 100,
				max_delay: 200,
			};

			await wait_for_package('test-pkg', '1.0.0', options);

			const wait_calls = vi.mocked(wait).mock.calls;
			for (const [delay] of wait_calls) {
				expect(delay).toBeLessThanOrEqual(220); // max_delay + 10% jitter
			}
		});

		it('applies jitter to delays', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 5) {
					return {stdout: ''} as any;
				}
				return {stdout: '1.0.0'} as any;
			});

			const options: WaitOptions = {
				initial_delay: 1000,
			};

			await wait_for_package('test-pkg', '1.0.0', options);

			const wait_calls = vi.mocked(wait).mock.calls;
			// Jitter should add up to 10% variance
			for (const [delay] of wait_calls) {
				// Base delay would be 1000, jitter adds 0-100ms
				expect(delay).toBeGreaterThanOrEqual(1000);
				expect(delay).toBeLessThan(6000); // With exponential backoff (1000 * 1.5^4 with jitter)
			}
		});

		it('throws after max_attempts', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);

			const options: WaitOptions = {
				max_attempts: 3,
				initial_delay: 10,
			};

			await expect(wait_for_package('test-pkg', '1.0.0', options)).rejects.toThrow(
				'test-pkg@1.0.0 not available after 3 attempts',
			);

			expect(spawn_out).toHaveBeenCalledTimes(3);
		});

		it('throws on timeout', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);
			vi.mocked(wait).mockImplementation(async (ms?: number) => {
				// Simulate time passing
				vi.spyOn(Date, 'now').mockReturnValue(Date.now() + (ms || 0));
			});

			const options: WaitOptions = {
				timeout: 500,
				initial_delay: 100,
			};

			await expect(wait_for_package('test-pkg', '1.0.0', options)).rejects.toThrow(
				'Timeout waiting for test-pkg@1.0.0 after 500ms',
			);
		});

		it('logs progress every 5 attempts', async () => {
			const log = create_mock_logger();
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 12) {
					return {stdout: ''} as any;
				}
				return {stdout: '1.0.0'} as any;
			});

			const options: WaitOptions = {
				initial_delay: 10,
			};

			await wait_for_package('test-pkg', '1.0.0', {...options, log});

			// Should log at attempts 5 and 10
			const progress_logs = log.info_calls.filter((log) => log.includes('Still waiting'));
			expect(progress_logs.length).toBe(2);
			expect(progress_logs[0]).toContain('attempt 5/30');
			expect(progress_logs[1]).toContain('attempt 10/30');
		});

		it('logs success message when package becomes available', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package('test-pkg', '1.0.0', {log});

			expect(log.info_calls.length).toBe(1);
			expect(log.info_calls[0]).toContain('test-pkg@1.0.0');
			expect(log.info_calls[0]).toContain('available on NPM');
		});

		it('uses default options when not specified', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package('test-pkg', '1.0.0');

			// Default: max_attempts = 30, should succeed immediately
			expect(spawn_out).toHaveBeenCalledTimes(1);
		});

		it('handles npm command errors during retry', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 3) {
					throw new Error('npm registry error');
				}
				return {stdout: '1.0.0'} as any;
			});

			await wait_for_package('test-pkg', '1.0.0');

			expect(spawn_out).toHaveBeenCalledTimes(3);
		});

		it('checks timeout before each attempt', async () => {
			let now = Date.now();
			vi.spyOn(Date, 'now').mockImplementation(() => now);

			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);
			vi.mocked(wait).mockImplementation(async (ms?: number) => {
				now += ms || 0;
			});

			const options: WaitOptions = {
				timeout: 200,
				initial_delay: 100,
			};

			await expect(wait_for_package('test-pkg', '1.0.0', options)).rejects.toThrow('Timeout');
		});

		it('handles very long package names', async () => {
			const long_name = '@very-long-scope/' + 'a'.repeat(100);
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package(long_name, '1.0.0');

			expect(spawn_out).toHaveBeenCalledWith('npm', ['view', `${long_name}@1.0.0`, 'version']);
		});
	});

	describe('get_package_info', () => {
		it('returns package info when package exists', async () => {
			const mock_data = {
				name: 'test-pkg',
				version: '1.2.3',
				description: 'Test package',
			};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await get_package_info('test-pkg');

			expect(result).toEqual({
				name: 'test-pkg',
				version: '1.2.3',
			});
			expect(spawn_out).toHaveBeenCalledWith('npm', ['view', 'test-pkg', '--json']);
		});

		it('returns null when npm command fails', async () => {
			vi.mocked(spawn_out).mockRejectedValue(new Error('package not found'));

			const result = await get_package_info('nonexistent-pkg');

			expect(result).toBeNull();
		});

		it('returns null when stdout is empty', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);

			const result = await get_package_info('test-pkg');

			expect(result).toBeNull();
		});

		it('returns null when stdout is undefined', async () => {
			vi.mocked(spawn_out).mockResolvedValue({} as any);

			const result = await get_package_info('test-pkg');

			expect(result).toBeNull();
		});

		it('logs debug message on error', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockRejectedValue(new Error('npm error'));

			await get_package_info('test-pkg', {log});

			expect(log.debug_calls.length).toBe(1);
			expect(log.debug_calls[0]).toContain('test-pkg');
			expect(log.debug_calls[0]).toContain('npm error');
		});

		it('handles scoped packages', async () => {
			const mock_data = {
				name: '@scope/package',
				version: '2.0.0',
			};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await get_package_info('@scope/package');

			expect(result?.name).toBe('@scope/package');
		});

		it('extracts only name and version from full package data', async () => {
			const mock_data = {
				name: 'test-pkg',
				version: '1.2.3',
				description: 'Many fields',
				dependencies: {},
				devDependencies: {},
				scripts: {},
				// ...many other fields
			};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await get_package_info('test-pkg');

			expect(result).toEqual({
				name: 'test-pkg',
				version: '1.2.3',
			});
			expect(Object.keys(result!)).toHaveLength(2);
		});

		it('handles invalid JSON response', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: 'not valid json'} as any);

			const result = await get_package_info('test-pkg');

			expect(result).toBeNull();
		});
	});

	describe('package_exists', () => {
		it('returns true when package exists', async () => {
			const mock_data = {name: 'test-pkg', version: '1.0.0'};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await package_exists('test-pkg');

			expect(result).toBe(true);
		});

		it('returns false when package does not exist', async () => {
			vi.mocked(spawn_out).mockRejectedValue(new Error('404'));

			const result = await package_exists('nonexistent-pkg');

			expect(result).toBe(false);
		});

		it('returns false when get_package_info returns null', async () => {
			vi.mocked(spawn_out).mockResolvedValue({} as any);

			const result = await package_exists('test-pkg');

			expect(result).toBe(false);
		});

		it('passes logger to get_package_info', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockRejectedValue(new Error('error'));

			await package_exists('test-pkg', {log});

			expect(log.debug_calls.length).toBe(1);
		});
	});
});
