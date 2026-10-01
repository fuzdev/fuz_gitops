import { assert, describe, test } from 'vitest';
import { assert_rejects, create_mock_logger } from '@fuzdev/fuz_util/testing.ts';

import {
	check_package_available,
	wait_for_package,
	type NpmRegistryDeps,
	type WaitOptions
} from '$lib/npm_registry.ts';

interface FakeRegistry {
	deps: NpmRegistryDeps;
	/** The args of each `npm` run, in order. */
	npm_calls: Array<Array<string>>;
	/** The milliseconds of each wait, in order. */
	waits: Array<number>;
}

/**
 * A fake registry over a fake clock: `respond` answers each `npm` run by its
 * 1-based attempt number with stdout or an error to throw, and each wait
 * advances the clock by its milliseconds.
 */
const create_fake_registry = (respond: (attempt: number) => string | Error): FakeRegistry => {
	const npm_calls: Array<Array<string>> = [];
	const waits: Array<number> = [];
	let clock = 0;
	return {
		npm_calls,
		waits,
		deps: {
			run_npm: async (args) => {
				npm_calls.push(args);
				const response = respond(npm_calls.length);
				if (response instanceof Error) throw response;
				return response;
			},
			wait: async (ms) => {
				waits.push(ms);
				clock += ms;
			},
			now: () => clock
		}
	};
};

/** Reports `version` from the `available_at`th attempt on, nothing before. */
const available_from = (available_at: number, version = '1.0.0') =>
	create_fake_registry((attempt) => (attempt >= available_at ? version : ''));

describe('check_package_available', () => {
	test('returns true when package version exists', async () => {
		const registry = create_fake_registry(() => '1.2.3');
		const result = await check_package_available('test-pkg', '1.2.3', {}, registry.deps);
		assert.strictEqual(result, true);
		assert.deepEqual(registry.npm_calls, [['view', 'test-pkg@1.2.3', 'version']]);
	});

	test('returns false when version does not match', async () => {
		const registry = create_fake_registry(() => '1.2.4');
		assert.strictEqual(
			await check_package_available('test-pkg', '1.2.3', {}, registry.deps),
			false
		);
	});

	test('returns false when npm fails to run', async () => {
		const registry = create_fake_registry(() => new Error('npm error'));
		assert.strictEqual(
			await check_package_available('test-pkg', '1.2.3', {}, registry.deps),
			false
		);
	});

	test('returns false when stdout is empty', async () => {
		const registry = create_fake_registry(() => '');
		assert.strictEqual(
			await check_package_available('test-pkg', '1.2.3', {}, registry.deps),
			false
		);
	});

	test('trims whitespace from stdout', async () => {
		const registry = create_fake_registry(() => '  1.2.3\n  ');
		assert.strictEqual(await check_package_available('test-pkg', '1.2.3', {}, registry.deps), true);
	});

	test('logs debug message on error', async () => {
		const log = create_mock_logger();
		const registry = create_fake_registry(() => new Error('network timeout'));

		await check_package_available('test-pkg', '1.2.3', { log }, registry.deps);

		assert.strictEqual(log.debug_calls.length, 1);
		assert.include(log.debug_calls[0] as string, 'test-pkg@1.2.3');
		assert.include(log.debug_calls[0] as string, 'network timeout');
	});

	test('handles scoped package names', async () => {
		const registry = create_fake_registry(() => '2.0.0');
		await check_package_available('@scope/package', '2.0.0', {}, registry.deps);
		assert.deepEqual(registry.npm_calls, [['view', '@scope/package@2.0.0', 'version']]);
	});

	test('handles prerelease versions', async () => {
		const registry = create_fake_registry(() => '1.0.0-beta.1');
		assert.strictEqual(
			await check_package_available('test-pkg', '1.0.0-beta.1', {}, registry.deps),
			true
		);
	});

	test('handles build metadata in versions', async () => {
		const registry = create_fake_registry(() => '1.0.0+build.123');
		assert.strictEqual(
			await check_package_available('test-pkg', '1.0.0+build.123', {}, registry.deps),
			true
		);
	});
});

describe('wait_for_package', () => {
	test('returns immediately when package is available', async () => {
		const registry = available_from(1);
		await wait_for_package('test-pkg', '1.0.0', {}, registry.deps);
		assert.strictEqual(registry.npm_calls.length, 1);
		assert.strictEqual(registry.waits.length, 0);
	});

	test('retries until package becomes available', async () => {
		const registry = available_from(3);
		await wait_for_package('test-pkg', '1.0.0', {}, registry.deps);
		assert.strictEqual(registry.npm_calls.length, 3);
		assert.strictEqual(registry.waits.length, 2);
	});

	test('applies exponential backoff', async () => {
		const registry = available_from(4);
		const options: WaitOptions = { initial_delay: 100, max_delay: 1000 };

		await wait_for_package('test-pkg', '1.0.0', options, registry.deps);

		const { waits } = registry;
		assert.strictEqual(waits.length, 3);
		// ~100ms, then ~150ms (100 * 1.5), then ~225ms (150 * 1.5), each plus up to 10% jitter
		assert.ok(waits[0]! >= 100 && waits[0]! < 120);
		assert.ok(waits[1]! >= 150 && waits[1]! < 180);
		assert.ok(waits[2]! >= 225 && waits[2]! < 270);
	});

	test('respects max_delay cap', async () => {
		const registry = available_from(10);
		const options: WaitOptions = { initial_delay: 100, max_delay: 200 };

		await wait_for_package('test-pkg', '1.0.0', options, registry.deps);

		for (const delay of registry.waits) {
			assert.ok(delay <= 200);
		}
	});

	test('applies jitter of up to 10% over the base delay', async () => {
		const registry = available_from(5);

		await wait_for_package('test-pkg', '1.0.0', { initial_delay: 1000 }, registry.deps);

		let base = 1000;
		for (const delay of registry.waits) {
			assert.ok(delay >= base && delay <= base * 1.1);
			base *= 1.5;
		}
	});

	test('throws after max_attempts', async () => {
		const registry = create_fake_registry(() => '');
		const options: WaitOptions = { max_attempts: 3, initial_delay: 10 };

		await assert_rejects(
			() => wait_for_package('test-pkg', '1.0.0', options, registry.deps),
			/test-pkg@1\.0\.0 not available after 3 attempts/
		);

		assert.strictEqual(registry.npm_calls.length, 3);
	});

	test('throws on timeout, checked before each attempt', async () => {
		const registry = create_fake_registry(() => '');
		const options: WaitOptions = { timeout: 500, initial_delay: 100 };

		await assert_rejects(
			() => wait_for_package('test-pkg', '1.0.0', options, registry.deps),
			/Timeout waiting for test-pkg@1\.0\.0 after 500ms/
		);

		// the clock passed the timeout during the last wait, and no attempt ran after it
		const elapsed = registry.waits.reduce((sum, ms) => sum + ms, 0);
		const elapsed_before_last = elapsed - registry.waits.at(-1)!;
		assert.ok(elapsed > 500);
		assert.ok(elapsed_before_last <= 500);
		assert.strictEqual(registry.npm_calls.length, registry.waits.length);
	});

	test('logs progress every 5 attempts', async () => {
		const log = create_mock_logger();
		const registry = available_from(12);

		await wait_for_package('test-pkg', '1.0.0', { initial_delay: 10, log }, registry.deps);

		const progress_logs = log.info_calls.filter((msg) => (msg as string).includes('Still waiting'));
		assert.strictEqual(progress_logs.length, 2);
		assert.include(progress_logs[0] as string, 'attempt 5/30');
		assert.include(progress_logs[1] as string, 'attempt 10/30');
	});

	test('logs success message when package becomes available', async () => {
		const log = create_mock_logger();
		const registry = available_from(1);

		await wait_for_package('test-pkg', '1.0.0', { log }, registry.deps);

		assert.strictEqual(log.info_calls.length, 1);
		assert.include(log.info_calls[0] as string, 'test-pkg@1.0.0');
		assert.include(log.info_calls[0] as string, 'available on NPM');
	});

	test('keeps retrying when npm fails to run', async () => {
		const registry = create_fake_registry((attempt) =>
			attempt < 3 ? new Error('npm registry error') : '1.0.0'
		);

		await wait_for_package('test-pkg', '1.0.0', {}, registry.deps);

		assert.strictEqual(registry.npm_calls.length, 3);
	});

	test('handles very long package names', async () => {
		const long_name = '@very-long-scope/' + 'a'.repeat(100);
		const registry = available_from(1);

		await wait_for_package(long_name, '1.0.0', {}, registry.deps);

		assert.deepEqual(registry.npm_calls, [['view', `${long_name}@1.0.0`, 'version']]);
	});
});
