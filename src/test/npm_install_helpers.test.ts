import {test} from 'vitest';

import {install_with_cache_healing} from '$lib/npm_install_helpers.js';
import type {NpmOperations} from '$lib/operations.js';
import {create_mock_gitops_ops, create_mock_repo} from './test_helpers.ts';

// Test: Install succeeds on first attempt (no cache healing needed)
test('install succeeds on first attempt', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});
	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => ({ok: true}),
			cache_clean: async () => ({ok: true}), // Should not be called
		} as NpmOperations,
	});

	// Should not throw
	await install_with_cache_healing(repo, ops);
	expect(true).toBe(true);
});

// Test: Install fails with ETARGET → cache clean → retry succeeds
test('ETARGET error triggers cache clean and retry', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	let install_call_count = 0;
	let cache_clean_called = false;

	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => {
				install_call_count++;
				if (install_call_count === 1) {
					// First attempt fails with ETARGET
					return {
						ok: false,
						message: 'Install failed',
						stderr: 'npm error code ETARGET\nnpm error notarget No matching version found',
					};
				} else {
					// Retry succeeds
					return {ok: true};
				}
			},
			cache_clean: async () => {
				cache_clean_called = true;
				return {ok: true};
			},
		} as NpmOperations,
	});

	await install_with_cache_healing(repo, ops);

	expect(install_call_count).toBe(2); // First attempt + retry
	expect(cache_clean_called).toBe(true);
});

// Test: ETARGET detection with various error message formats
test('detects ETARGET in various formats', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	const test_cases = [
		// Uppercase ETARGET
		{stderr: 'npm error code ETARGET', message: 'Failed'},
		// Lowercase
		{stderr: 'npm error code etarget', message: 'Failed'},
		// notarget
		{stderr: 'npm error notarget No matching version found', message: 'Install failed'},
		// ETARGET in message
		{stderr: '', message: 'ETARGET: package not found'},
		// Mixed case
		{stderr: 'No Matching Version Found for package', message: 'Error'},
	];

	for (const {stderr, message: error_message} of test_cases) {
		let cache_clean_called = false;
		let install_call_count = 0;

		const ops = create_mock_gitops_ops({
			npm: {
				install: async () => {
					install_call_count++;
					if (install_call_count === 1) {
						return {ok: false, message: error_message, stderr};
					} else {
						return {ok: true};
					}
				},
				cache_clean: async () => {
					cache_clean_called = true;
					return {ok: true};
				},
			} as NpmOperations,
		});

		await install_with_cache_healing(repo, ops);
		expect(cache_clean_called).toBe(true);
	}
});

// Test: Non-ETARGET error fails immediately without cache clean
test('non-ETARGET error fails immediately', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	let cache_clean_called = false;

	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => ({
				ok: false,
				message: 'Network error',
				stderr: 'npm ERR! network timeout',
			}),
			cache_clean: async () => {
				cache_clean_called = true;
				return {ok: true};
			},
		} as NpmOperations,
	});

	await expect(async () => {
		await install_with_cache_healing(repo, ops);
	}).rejects.toThrow('Failed to install dependencies in test-pkg');

	// Cache clean should NOT have been called
	expect(cache_clean_called).toBe(false);
});

// Test: ETARGET error but cache clean fails
test('ETARGET triggers cache clean but clean fails', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => ({
				ok: false,
				message: 'Install failed',
				stderr: 'npm error code ETARGET',
			}),
			cache_clean: async () => ({
				ok: false,
				message: 'Cache clean failed: permission denied',
			}),
		} as NpmOperations,
	});

	await expect(async () => {
		await install_with_cache_healing(repo, ops);
	}).rejects.toThrow('Failed to clean npm cache: Cache clean failed: permission denied');
});

// Test: ETARGET error, cache clean succeeds, but retry install fails
test('cache clean succeeds but retry install fails', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	let install_call_count = 0;

	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => {
				install_call_count++;
				if (install_call_count === 1) {
					return {
						ok: false,
						message: 'Install failed',
						stderr: 'npm error code ETARGET',
					};
				} else {
					// Retry also fails (different error)
					return {
						ok: false,
						message: 'Still failing',
						stderr: 'npm ERR! permission denied',
					};
				}
			},
			cache_clean: async () => ({ok: true}),
		} as NpmOperations,
	});

	await expect(async () => {
		await install_with_cache_healing(repo, ops);
	}).rejects.toThrow('Failed to install dependencies after cache clean in test-pkg');
});

// Test: Error message includes stderr details
test('error message includes stderr details', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => ({
				ok: false,
				message: 'Install failed',
				stderr: 'npm ERR! detailed error log\nnpm ERR! more details',
			}),
		} as NpmOperations,
	});

	await expect(async () => {
		await install_with_cache_healing(repo, ops);
	}).rejects.toThrow(/Install failed\nnpm ERR! detailed error log/);
});

// Test: Successful install with no stderr
test('successful install with no stderr', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => ({ok: true}),
		} as NpmOperations,
	});

	// Should complete without error
	await install_with_cache_healing(repo, ops);
	expect(true).toBe(true);
});

// Test: Multiple ETARGET variations in same error message
test('detects ETARGET with multiple indicators', async ({expect}) => {
	const repo = create_mock_repo({name: 'test-pkg'});

	let cache_clean_called = false;

	// First install fails, should trigger cache clean, retry also fails to ensure detection works
	const ops = create_mock_gitops_ops({
		npm: {
			install: async () => ({
				ok: false,
				message: 'npm error code ETARGET',
				stderr: 'npm error notarget No matching version found',
			}),
			cache_clean: async () => {
				cache_clean_called = true;
				return {ok: true};
			},
		} as NpmOperations,
	});

	await expect(async () => {
		await install_with_cache_healing(repo, ops);
	}).rejects.toThrow();

	expect(cache_clean_called).toBe(true);
});
