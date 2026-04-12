import {assert, describe, test, vi, beforeEach, afterEach} from 'vitest';
import {spawn_out} from '@fuzdev/fuz_util/process.js';
import {wait} from '@fuzdev/fuz_util/async.js';
import {assert_rejects, create_mock_logger} from '@fuzdev/fuz_util/testing.js';

import {
	check_package_available,
	wait_for_package,
	get_package_info,
	package_exists,
	type WaitOptions,
} from '$lib/npm_registry.js';

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
		test('returns true when package version exists', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.2.3'} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			assert.strictEqual(result, true);
			assert.deepEqual(vi.mocked(spawn_out).mock.calls[0], [
				'npm',
				['view', 'test-pkg@1.2.3', 'version'],
			]);
		});

		test('returns false when version does not match', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.2.4'} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			assert.strictEqual(result, false);
		});

		test('returns false when npm command fails', async () => {
			vi.mocked(spawn_out).mockRejectedValue(new Error('npm error'));

			const result = await check_package_available('test-pkg', '1.2.3');

			assert.strictEqual(result, false);
		});

		test('returns false when stdout is empty', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			assert.strictEqual(result, false);
		});

		test('returns false when stdout is undefined', async () => {
			vi.mocked(spawn_out).mockResolvedValue({} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			assert.strictEqual(result, false);
		});

		test('trims whitespace from stdout', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '  1.2.3\n  '} as any);

			const result = await check_package_available('test-pkg', '1.2.3');

			assert.strictEqual(result, true);
		});

		test('logs debug message on error', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockRejectedValue(new Error('network timeout'));

			await check_package_available('test-pkg', '1.2.3', {log});

			assert.strictEqual(log.debug_calls.length, 1);
			assert.ok((log.debug_calls[0] as string).includes('test-pkg@1.2.3'));
			assert.ok((log.debug_calls[0] as string).includes('network timeout'));
		});

		test('handles scoped package names', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '2.0.0'} as any);

			await check_package_available('@scope/package', '2.0.0');

			assert.deepEqual(vi.mocked(spawn_out).mock.calls[0], [
				'npm',
				['view', '@scope/package@2.0.0', 'version'],
			]);
		});

		test('handles prerelease versions', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0-beta.1'} as any);

			const result = await check_package_available('test-pkg', '1.0.0-beta.1');

			assert.strictEqual(result, true);
		});

		test('handles build metadata in versions', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0+build.123'} as any);

			const result = await check_package_available('test-pkg', '1.0.0+build.123');

			assert.strictEqual(result, true);
		});
	});

	describe('wait_for_package', () => {
		test('returns immediately when package is available', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package('test-pkg', '1.0.0');

			assert.strictEqual(vi.mocked(spawn_out).mock.calls.length, 1);
			assert.strictEqual(vi.mocked(wait).mock.calls.length, 0);
		});

		test('retries until package becomes available', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 3) {
					return {stdout: ''} as any;
				}
				return {stdout: '1.0.0'} as any;
			});

			await wait_for_package('test-pkg', '1.0.0');

			assert.strictEqual(vi.mocked(spawn_out).mock.calls.length, 3);
			assert.strictEqual(vi.mocked(wait).mock.calls.length, 2);
		});

		test('applies exponential backoff', async () => {
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
			assert.strictEqual(wait_calls.length, 3);

			// First delay: ~100ms (+ jitter)
			assert.ok(wait_calls[0]![0]! >= 100);
			assert.ok(wait_calls[0]![0]! < 120);

			// Second delay: ~150ms (100 * 1.5 + jitter)
			assert.ok(wait_calls[1]![0]! >= 150);
			assert.ok(wait_calls[1]![0]! < 180);

			// Third delay: ~225ms (150 * 1.5 + jitter)
			assert.ok(wait_calls[2]![0]! >= 225);
			assert.ok(wait_calls[2]![0]! < 270);
		});

		test('respects max_delay cap', async () => {
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
				assert.ok(delay! <= 220); // max_delay + 10% jitter
			}
		});

		test('applies jitter to delays', async () => {
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
				assert.ok(delay! >= 1000);
				assert.ok(delay! < 6000); // With exponential backoff (1000 * 1.5^4 with jitter)
			}
		});

		test('throws after max_attempts', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);

			const options: WaitOptions = {
				max_attempts: 3,
				initial_delay: 10,
			};

			await assert_rejects(
				() => wait_for_package('test-pkg', '1.0.0', options),
				/test-pkg@1\.0\.0 not available after 3 attempts/,
			);

			assert.strictEqual(vi.mocked(spawn_out).mock.calls.length, 3);
		});

		test('throws on timeout', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);
			vi.mocked(wait).mockImplementation(async (ms?: number) => {
				// Simulate time passing
				vi.spyOn(Date, 'now').mockReturnValue(Date.now() + (ms || 0));
			});

			const options: WaitOptions = {
				timeout: 500,
				initial_delay: 100,
			};

			await assert_rejects(
				() => wait_for_package('test-pkg', '1.0.0', options),
				/Timeout waiting for test-pkg@1\.0\.0 after 500ms/,
			);
		});

		test('logs progress every 5 attempts', async () => {
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
			const progress_logs = log.info_calls.filter((msg) =>
				(msg as string).includes('Still waiting'),
			);
			assert.strictEqual(progress_logs.length, 2);
			assert.ok((progress_logs[0] as string).includes('attempt 5/30'));
			assert.ok((progress_logs[1] as string).includes('attempt 10/30'));
		});

		test('logs success message when package becomes available', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package('test-pkg', '1.0.0', {log});

			assert.strictEqual(log.info_calls.length, 1);
			assert.ok((log.info_calls[0] as string).includes('test-pkg@1.0.0'));
			assert.ok((log.info_calls[0] as string).includes('available on NPM'));
		});

		test('uses default options when not specified', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package('test-pkg', '1.0.0');

			// Default: max_attempts = 30, should succeed immediately
			assert.strictEqual(vi.mocked(spawn_out).mock.calls.length, 1);
		});

		test('handles npm command errors during retry', async () => {
			let attempt = 0;
			vi.mocked(spawn_out).mockImplementation(async () => {
				attempt++;
				if (attempt < 3) {
					throw new Error('npm registry error');
				}
				return {stdout: '1.0.0'} as any;
			});

			await wait_for_package('test-pkg', '1.0.0');

			assert.strictEqual(vi.mocked(spawn_out).mock.calls.length, 3);
		});

		test('checks timeout before each attempt', async () => {
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

			await assert_rejects(() => wait_for_package('test-pkg', '1.0.0', options), /Timeout/);
		});

		test('handles very long package names', async () => {
			const long_name = '@very-long-scope/' + 'a'.repeat(100);
			vi.mocked(spawn_out).mockResolvedValue({stdout: '1.0.0'} as any);

			await wait_for_package(long_name, '1.0.0');

			assert.deepEqual(vi.mocked(spawn_out).mock.calls[0], [
				'npm',
				['view', `${long_name}@1.0.0`, 'version'],
			]);
		});
	});

	describe('get_package_info', () => {
		test('returns package info when package exists', async () => {
			const mock_data = {
				name: 'test-pkg',
				version: '1.2.3',
				description: 'Test package',
			};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await get_package_info('test-pkg');

			assert.deepEqual(result, {
				name: 'test-pkg',
				version: '1.2.3',
			});
			assert.deepEqual(vi.mocked(spawn_out).mock.calls[0], ['npm', ['view', 'test-pkg', '--json']]);
		});

		test('returns null when npm command fails', async () => {
			vi.mocked(spawn_out).mockRejectedValue(new Error('package not found'));

			const result = await get_package_info('nonexistent-pkg');

			assert.strictEqual(result, null);
		});

		test('returns null when stdout is empty', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: ''} as any);

			const result = await get_package_info('test-pkg');

			assert.strictEqual(result, null);
		});

		test('returns null when stdout is undefined', async () => {
			vi.mocked(spawn_out).mockResolvedValue({} as any);

			const result = await get_package_info('test-pkg');

			assert.strictEqual(result, null);
		});

		test('logs debug message on error', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockRejectedValue(new Error('npm error'));

			await get_package_info('test-pkg', {log});

			assert.strictEqual(log.debug_calls.length, 1);
			assert.ok((log.debug_calls[0] as string).includes('test-pkg'));
			assert.ok((log.debug_calls[0] as string).includes('npm error'));
		});

		test('handles scoped packages', async () => {
			const mock_data = {
				name: '@scope/package',
				version: '2.0.0',
			};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await get_package_info('@scope/package');

			assert.strictEqual(result?.name, '@scope/package');
		});

		test('extracts only name and version from full package data', async () => {
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

			assert.deepEqual(result, {
				name: 'test-pkg',
				version: '1.2.3',
			});
			assert.strictEqual(Object.keys(result!).length, 2);
		});

		test('handles invalid JSON response', async () => {
			vi.mocked(spawn_out).mockResolvedValue({stdout: 'not valid json'} as any);

			const result = await get_package_info('test-pkg');

			assert.strictEqual(result, null);
		});
	});

	describe('package_exists', () => {
		test('returns true when package exists', async () => {
			const mock_data = {name: 'test-pkg', version: '1.0.0'};
			vi.mocked(spawn_out).mockResolvedValue({stdout: JSON.stringify(mock_data)} as any);

			const result = await package_exists('test-pkg');

			assert.strictEqual(result, true);
		});

		test('returns false when package does not exist', async () => {
			vi.mocked(spawn_out).mockRejectedValue(new Error('404'));

			const result = await package_exists('nonexistent-pkg');

			assert.strictEqual(result, false);
		});

		test('returns false when get_package_info returns null', async () => {
			vi.mocked(spawn_out).mockResolvedValue({} as any);

			const result = await package_exists('test-pkg');

			assert.strictEqual(result, false);
		});

		test('passes logger to get_package_info', async () => {
			const log = create_mock_logger();
			vi.mocked(spawn_out).mockRejectedValue(new Error('error'));

			await package_exists('test-pkg', {log});

			assert.strictEqual(log.debug_calls.length, 1);
		});
	});
});
