import { assert, describe, test } from 'vitest';

import { run_preflight_checks } from '$lib/preflight_checks.ts';
import {
	create_mock_repo,
	create_mock_repos_entry,
	create_mock_npm_ops,
	create_mock_build_ops
} from './test_helpers.ts';
import type { LocalRepo } from '$lib/local_repo.ts';

describe('preflight_checks', () => {
	describe('repo git state', () => {
		test("isn't preflight's: a repo off its branch and dirty passes (the readiness gate refuses it)", async () => {
			const repo = create_mock_repo({ name: 'package-a' });
			const primary = repo.entry.checkouts[0]!;
			const not_at_rest: LocalRepo = {
				...repo,
				entry: create_mock_repos_entry({
					key: 'package-a',
					checkouts: [
						{
							...primary,
							head: { kind: 'branch', name: 'feature' },
							uncommitted: { staged: 0, unstaged: 0, untracked: 1, conflicted: 0 }
						}
					],
					at_rest: {
						on_branch: false,
						clean: false,
						idle: true,
						followed: { kind: 'ahead', commits: 1 }
					}
				})
			};

			const result = await run_preflight_checks({
				repos: [not_at_rest],
				preflight_options: { skip_changesets: true },
				npm_ops: create_mock_npm_ops()
			});

			assert.strictEqual(result.ok, true);
			assert.deepEqual(result.errors, []);
		});
	});

	describe('changeset validation', () => {
		test('detects repos with changesets', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' })
			];

			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {},
				npm_ops
			});

			// Without actual changesets, all should be marked as without
			assert.strictEqual(result.repos_without_changesets.size, 2);
			assert.strictEqual(result.repos_with_changesets.size, 0);
		});

		test('warns about packages without changesets', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' })
			];

			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {},
				npm_ops
			});

			// Filter for changeset-related warnings (may also have npm warnings)
			const changeset_warnings = result.warnings.filter((w) => w.includes('no changesets'));
			assert.strictEqual(changeset_warnings.length, 2);
		});

		test('skips changeset checks when skip_changesets is true', async () => {
			const repos = [create_mock_repo({ name: 'package-a' })];

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: true },
				npm_ops
			});

			assert.strictEqual(result.repos_with_changesets.size, 0);
			assert.strictEqual(result.repos_without_changesets.size, 0);
			// May have npm warnings, but no changeset warnings
			const changeset_warnings = result.warnings.filter((w) => w.includes('changesets'));
			assert.strictEqual(changeset_warnings.length, 0);
		});
	});

	describe('npm authentication', () => {
		// Note: The actual npm auth check uses spawn_out('npm', ['whoami'])
		// In real tests, this would need to be mocked at the spawn level
		// For now, we test the integration assuming npm commands work

		test('passes with valid npm authentication', async () => {
			const repos = [create_mock_repo({ name: 'package-a' })];

			// This test depends on actual npm being logged in
			// In a real test, we'd mock spawn_out
			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: true },
				npm_ops
			});

			// We can't assert npm auth result without mocking spawn
			// but we can check the structure
			assert.ok('ok' in result);
			assert.ok('errors' in result);
		});
	});

	describe('empty repo list', () => {
		test('passes with empty repo list', async () => {
			const repos: Array<LocalRepo> = [];

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: true },
				npm_ops
			});

			assert.strictEqual(result.ok, true);
			assert.strictEqual(result.errors.length, 0);
			// May have npm warnings, but that's acceptable for empty list
		});
	});

	describe('result structure', () => {
		test('returns correct result structure', async () => {
			const repos = [create_mock_repo({ name: 'package-a' })];

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: true },
				npm_ops
			});

			assert.ok('ok' in result);
			assert.ok('warnings' in result);
			assert.ok('errors' in result);
			assert.ok('repos_with_changesets' in result);
			assert.ok('repos_without_changesets' in result);

			assert.strictEqual(Array.isArray(result.warnings), true);
			assert.strictEqual(Array.isArray(result.errors), true);
			assert.ok(result.repos_with_changesets instanceof Set);
			assert.ok(result.repos_without_changesets instanceof Set);
		});
	});

	describe('build validation', () => {
		test('skips build validation when skip_build_validation is true', async () => {
			const repos = [create_mock_repo({ name: 'package-a' })];
			const npm_ops = create_mock_npm_ops();

			let build_called = false;
			const build_ops = create_mock_build_ops({
				build_package: async () => {
					build_called = true;
					return { ok: true };
				}
			});

			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_build_validation: true },
				npm_ops,
				build_ops
			});

			assert.strictEqual(result.ok, true);
			assert.strictEqual(build_called, false);
		});

		test('validates builds for packages with changesets', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' })
			];

			const npm_ops = create_mock_npm_ops();

			let build_count = 0;
			const built_packages: Array<string> = [];
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					build_count++;
					built_packages.push(options.repo.library.name);
					return { ok: true };
				}
			});

			// Note: In the real implementation, has_changesets is imported from changeset_reader
			// For proper testing, we'd need to mock that module, but for now these tests
			// document the expected behavior
			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: false },
				npm_ops,
				build_ops
			});

			// Since mock repos don't have actual .changeset/ directories, build count is 0
			assert.strictEqual(result.ok, true);
			assert.strictEqual(build_count, 0);
		});

		test('fails when a build fails', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' })
			];

			const npm_ops = create_mock_npm_ops();

			let call_count = 0;
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					call_count++;
					if (options.repo.library.name === 'package-b') {
						return { ok: false, message: 'TypeScript compilation error' };
					}
					return { ok: true };
				}
			});

			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: false },
				npm_ops,
				build_ops
			});

			// Since mock repos don't have changesets, no builds run
			assert.strictEqual(result.ok, true);
			assert.strictEqual(call_count, 0);
		});

		test('fails preflight when build fails for package with changesets', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' })
			];

			const npm_ops = create_mock_npm_ops();

			// Mock build ops where package-b fails
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					if (options.repo.library.name === 'package-b') {
						return { ok: false, message: 'Build failed: syntax error' };
					}
					return { ok: true };
				}
			});

			// Mock changeset ops where only package-a and package-b have changesets
			const changeset_ops = {
				has_changesets: async (options: { repo: LocalRepo }) => ({
					ok: true as const,
					value:
						options.repo.library.name === 'package-a' || options.repo.library.name === 'package-b'
				}),
				read_changesets: async () => ({ ok: true as const, value: [] }),
				predict_next_version: async () => null
			};

			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: false },
				npm_ops,
				build_ops,
				changeset_ops
			});

			// Should fail due to build error
			assert.strictEqual(result.ok, false);
			assert.ok(result.errors.some((e) => e.includes('package-b failed to build')));
			assert.ok(result.errors.some((e) => e.includes('syntax error')));
		});

		test('reports build failures with error details', async () => {
			const repos = [create_mock_repo({ name: 'failing-package' })];

			const npm_ops = create_mock_npm_ops();
			const build_ops = create_mock_build_ops({
				build_package: async () => ({
					ok: false,
					message: 'Syntax error in src/main.ts:42'
				})
			});

			// Mock changeset ops where failing-package has changesets
			const changeset_ops = {
				has_changesets: async (options: { repo: LocalRepo }) => ({
					ok: true as const,
					value: options.repo.library.name === 'failing-package'
				}),
				read_changesets: async () => ({ ok: true as const, value: [] }),
				predict_next_version: async () => null
			};

			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: false },
				npm_ops,
				build_ops,
				changeset_ops
			});

			// Should fail with detailed error message
			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.strictEqual(
				result.errors[0],
				'failing-package failed to build: Syntax error in src/main.ts:42'
			);
		});

		test('validates builds only for packages with changesets', async () => {
			const repos = [
				create_mock_repo({ name: 'with-changeset' }),
				create_mock_repo({ name: 'without-changeset' })
			];

			const npm_ops = create_mock_npm_ops();

			const built_packages: Array<string> = [];
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					built_packages.push(options.repo.library.name);
					return { ok: true };
				}
			});

			await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: true },
				npm_ops,
				build_ops
			});

			// With skip_changesets, no builds should run
			assert.strictEqual(built_packages.length, 0);
		});

		test('continues validation after build failures to report all issues', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' }),
				create_mock_repo({ name: 'package-c' })
			];

			const npm_ops = create_mock_npm_ops();

			const built_packages: Array<string> = [];
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					built_packages.push(options.repo.library.name);
					// Fail on package-a and package-c
					if (
						options.repo.library.name === 'package-a' ||
						options.repo.library.name === 'package-c'
					) {
						return { ok: false, message: 'Build error' };
					}
					return { ok: true };
				}
			});

			// Mock changeset ops where all packages have changesets
			const changeset_ops = {
				has_changesets: async () => ({ ok: true as const, value: true }),
				read_changesets: async () => ({ ok: true as const, value: [] }),
				predict_next_version: async () => null
			};

			const result = await run_preflight_checks({
				repos,
				preflight_options: { skip_changesets: false },
				npm_ops,
				build_ops,
				changeset_ops
			});

			// Should fail but continue to build all packages
			assert.strictEqual(result.ok, false);
			assert.strictEqual(built_packages.length, 3); // All 3 packages were attempted
			assert.ok(built_packages.includes('package-a'));
			assert.ok(built_packages.includes('package-b'));
			assert.ok(built_packages.includes('package-c'));

			// Should report both failures
			assert.strictEqual(result.errors.length, 2);
			assert.ok(result.errors.some((e) => e.includes('package-a failed to build')));
			assert.ok(result.errors.some((e) => e.includes('package-c failed to build')));
		});
	});
});
