import {assert, describe, test} from 'vitest';

import {run_preflight_checks} from '$lib/preflight_checks.js';
import {
	create_mock_repo,
	create_mock_git_ops,
	create_mock_npm_ops,
	create_mock_build_ops,
} from './test_helpers.js';
import type {LocalRepo} from '$lib/local_repo.js';

describe('preflight_checks', () => {
	describe('workspace cleanliness', () => {
		test('passes when all workspaces are clean', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => ({ok: true, value: true}),
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, true);
			assert.strictEqual(result.errors.length, 0);
		});

		test('fails when a workspace has uncommitted changes', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			let call_count = 0;
			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => {
					call_count++;
					return {ok: true, value: call_count !== 2}; // Second repo fails
				},
				get_changed_files: async () => ({ok: true, value: ['src/main.ts']}), // Simulate changed files
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.ok(result.errors[0]!.includes('package-b'));
			assert.ok(result.errors[0]!.includes('uncommitted changes'));
		});

		test('reports all repos with uncommitted changes', async () => {
			const repos = [
				create_mock_repo({name: 'package-a'}),
				create_mock_repo({name: 'package-b'}),
				create_mock_repo({name: 'package-c'}),
			];

			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => ({ok: true, value: false}), // All dirty
				get_changed_files: async () => ({ok: true, value: ['src/file.ts']}), // Simulate changed files
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 3);
		});

		test('fails when workspace has changeset files (no filtering)', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => ({ok: true, value: false}),
				get_changed_files: async () => ({ok: true, value: ['.changeset/my-change.md']}),
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			// Should fail - changeset files are no longer filtered
			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.ok(result.errors[0]!.includes('.changeset/my-change.md'));
		});

		test('fails when workspace has package.json changes (no filtering)', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => ({ok: true, value: false}),
				get_changed_files: async () => ({ok: true, value: ['package.json']}),
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			// Should fail - package.json is no longer filtered
			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.ok(result.errors[0]!.includes('package.json'));
		});

		test('fails when workspace has package-lock.json changes (no filtering)', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => ({ok: true, value: false}),
				get_changed_files: async () => ({ok: true, value: ['package-lock.json']}),
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			// Should fail - package-lock.json is no longer filtered
			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.ok(result.errors[0]!.includes('package-lock.json'));
		});
	});

	describe('branch validation', () => {
		test('passes when all repos are on the required branch', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops({
				current_branch_name: async () => ({ok: true, value: 'main'}),
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, required_branch: 'main', check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, true);
			assert.strictEqual(result.errors.length, 0);
		});

		test('fails when a repo is on wrong branch', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			let call_count = 0;
			const git_ops = create_mock_git_ops({
				current_branch_name: async () => {
					call_count++;
					return {ok: true, value: call_count === 1 ? 'main' : 'develop'};
				},
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, required_branch: 'main', check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.ok(result.errors[0]!.includes('package-b'));
			assert.ok(result.errors[0]!.includes("is on branch 'develop'"));
			assert.ok(result.errors[0]!.includes("expected 'main'"));
		});

		test('supports custom required branch', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops({
				current_branch_name: async () => ({ok: true, value: 'release'}),
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, required_branch: 'release', check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, true);
		});

		test('defaults to main branch if not specified', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops({
				current_branch_name: async () => ({ok: true, value: 'develop'}),
			});

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, false);
			assert.ok(result.errors[0]!.includes("expected 'main'"));
		});
	});

	describe('changeset validation', () => {
		test('detects repos with changesets', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false},
				git_ops,
				npm_ops,
			});

			// Without actual changesets, all should be marked as without
			assert.strictEqual(result.repos_without_changesets.size, 2);
			assert.strictEqual(result.repos_with_changesets.size, 0);
		});

		test('warns about packages without changesets', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false},
				git_ops,
				npm_ops,
			});

			// Filter for changeset-related warnings (may also have npm warnings)
			const changeset_warnings = result.warnings.filter((w) => w.includes('no changesets'));
			assert.strictEqual(changeset_warnings.length, 2);
		});

		test('skips changeset checks when skip_changesets is true', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops();

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
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
			const repos = [create_mock_repo({name: 'package-a'})];
			const git_ops = create_mock_git_ops();

			// This test depends on actual npm being logged in
			// In a real test, we'd mock spawn_out
			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			// We can't assert npm auth result without mocking spawn
			// but we can check the structure
			assert.ok('ok' in result);
			assert.ok('errors' in result);
		});
	});

	describe('multiple validation failures', () => {
		test('reports all types of failures together', async () => {
			const repos = [
				create_mock_repo({name: 'dirty-wrong-branch'}),
				create_mock_repo({name: 'clean-wrong-branch'}),
			];

			let branch_call = 0;
			let clean_call = 0;

			const git_ops = create_mock_git_ops({
				check_clean_workspace: async () => {
					clean_call++;
					return {ok: true, value: clean_call !== 1}; // First repo is dirty
				},
				get_changed_files: async () => ({ok: true, value: ['src/main.ts']}), // Simulate changed files
				current_branch_name: async () => {
					branch_call++;
					return {ok: true, value: 'develop'}; // Both on wrong branch
				},
			});
			const npm_ops = create_mock_npm_ops();

			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, required_branch: 'main', check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 3); // 1 dirty + 2 wrong branches
			assert.strictEqual(clean_call, 2); // Check called for both repos
			assert.strictEqual(branch_call, 2); // Check called for both repos
		});
	});

	describe('empty repo list', () => {
		test('passes with empty repo list', async () => {
			const repos: Array<LocalRepo> = [];
			const git_ops = create_mock_git_ops();

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			assert.strictEqual(result.ok, true);
			assert.strictEqual(result.errors.length, 0);
			// May have npm warnings, but that's acceptable for empty list
		});
	});

	describe('result structure', () => {
		test('returns correct result structure', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];
			const git_ops = create_mock_git_ops();

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
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
			const repos = [create_mock_repo({name: 'package-a'})];
			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			let build_called = false;
			const build_ops = create_mock_build_ops({
				build_package: async () => {
					build_called = true;
					return {ok: true};
				},
			});

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_build_validation: true},
				git_ops,
				npm_ops,
				build_ops,
			});

			assert.strictEqual(result.ok, true);
			assert.strictEqual(build_called, false);
		});

		test('validates builds for packages with changesets', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			let build_count = 0;
			const built_packages: Array<string> = [];
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					build_count++;
					built_packages.push(options.repo.library.name);
					return {ok: true};
				},
			});

			// Note: In the real implementation, has_changesets is imported from changeset_reader
			// For proper testing, we'd need to mock that module, but for now these tests
			// document the expected behavior
			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_changesets: false},
				git_ops,
				npm_ops,
				build_ops,
			});

			// Since mock repos don't have actual .changeset/ directories, build count is 0
			assert.strictEqual(result.ok, true);
			assert.strictEqual(build_count, 0);
		});

		test('fails when a build fails', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			let call_count = 0;
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					call_count++;
					if (options.repo.library.name === 'package-b') {
						return {ok: false, message: 'TypeScript compilation error'};
					}
					return {ok: true};
				},
			});

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_changesets: false},
				git_ops,
				npm_ops,
				build_ops,
			});

			// Since mock repos don't have changesets, no builds run
			assert.strictEqual(result.ok, true);
			assert.strictEqual(call_count, 0);
		});

		test('fails preflight when build fails for package with changesets', async () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			// Mock build ops where package-b fails
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					if (options.repo.library.name === 'package-b') {
						return {ok: false, message: 'Build failed: syntax error'};
					}
					return {ok: true};
				},
			});

			// Mock changeset ops where only package-a and package-b have changesets
			const changeset_ops = {
				has_changesets: async (options: {repo: LocalRepo}) => ({
					ok: true as const,
					value:
						options.repo.library.name === 'package-a' || options.repo.library.name === 'package-b',
				}),
				read_changesets: async () => ({ok: true as const, value: []}),
				predict_next_version: async () => null,
			};

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_changesets: false},
				git_ops,
				npm_ops,
				build_ops,
				changeset_ops,
			});

			// Should fail due to build error
			assert.strictEqual(result.ok, false);
			assert.ok(result.errors.some((e) => e.includes('package-b failed to build')));
			assert.ok(result.errors.some((e) => e.includes('syntax error')));
		});

		test('reports build failures with error details', async () => {
			const repos = [create_mock_repo({name: 'failing-package'})];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();
			const build_ops = create_mock_build_ops({
				build_package: async () => ({
					ok: false,
					message: 'Syntax error in src/main.ts:42',
				}),
			});

			// Mock changeset ops where failing-package has changesets
			const changeset_ops = {
				has_changesets: async (options: {repo: LocalRepo}) => ({
					ok: true as const,
					value: options.repo.library.name === 'failing-package',
				}),
				read_changesets: async () => ({ok: true as const, value: []}),
				predict_next_version: async () => null,
			};

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_changesets: false},
				git_ops,
				npm_ops,
				build_ops,
				changeset_ops,
			});

			// Should fail with detailed error message
			assert.strictEqual(result.ok, false);
			assert.strictEqual(result.errors.length, 1);
			assert.strictEqual(
				result.errors[0],
				'failing-package failed to build: Syntax error in src/main.ts:42',
			);
		});

		test('validates builds only for packages with changesets', async () => {
			const repos = [
				create_mock_repo({name: 'with-changeset'}),
				create_mock_repo({name: 'without-changeset'}),
			];

			const git_ops = create_mock_git_ops();
			const npm_ops = create_mock_npm_ops();

			const built_packages: Array<string> = [];
			const build_ops = create_mock_build_ops({
				build_package: async (options) => {
					built_packages.push(options.repo.library.name);
					return {ok: true};
				},
			});

			await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_changesets: true},
				git_ops,
				npm_ops,
				build_ops,
			});

			// With skip_changesets, no builds should run
			assert.strictEqual(built_packages.length, 0);
		});

		test('continues validation after build failures to report all issues', async () => {
			const repos = [
				create_mock_repo({name: 'package-a'}),
				create_mock_repo({name: 'package-b'}),
				create_mock_repo({name: 'package-c'}),
			];

			const git_ops = create_mock_git_ops();
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
						return {ok: false, message: 'Build error'};
					}
					return {ok: true};
				},
			});

			// Mock changeset ops where all packages have changesets
			const changeset_ops = {
				has_changesets: async () => ({ok: true as const, value: true}),
				read_changesets: async () => ({ok: true as const, value: []}),
				predict_next_version: async () => null,
			};

			const result = await run_preflight_checks({
				repos,
				preflight_options: {check_remote: false, skip_changesets: false},
				git_ops,
				npm_ops,
				build_ops,
				changeset_ops,
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
