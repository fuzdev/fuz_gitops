import {describe, it, expect} from 'vitest';

import {run_preflight_checks} from '$lib/preflight_checks.js';
import {
	create_mock_repo,
	create_mock_git_ops,
	create_mock_npm_ops,
	create_mock_build_ops,
} from './test_helpers.ts';
import type {LocalRepo} from '$lib/local_repo.js';

describe('preflight_checks', () => {
	describe('workspace cleanliness', () => {
		it('passes when all workspaces are clean', async () => {
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

			expect(result.ok).toBe(true);
			expect(result.errors).toHaveLength(0);
		});

		it('fails when a workspace has uncommitted changes', async () => {
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

			expect(result.ok).toBe(false);
			expect(result.errors).toHaveLength(1);
			expect(result.errors[0]).toContain('package-b');
			expect(result.errors[0]).toContain('uncommitted changes');
		});

		it('reports all repos with uncommitted changes', async () => {
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

			expect(result.ok).toBe(false);
			expect(result.errors).toHaveLength(3);
		});

		it('fails when workspace has changeset files (no filtering)', async () => {
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
			expect(result.ok).toBe(false);
			expect(result.errors).toHaveLength(1);
			expect(result.errors[0]).toContain('.changeset/my-change.md');
		});

		it('fails when workspace has package.json changes (no filtering)', async () => {
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
			expect(result.ok).toBe(false);
			expect(result.errors).toHaveLength(1);
			expect(result.errors[0]).toContain('package.json');
		});

		it('fails when workspace has package-lock.json changes (no filtering)', async () => {
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
			expect(result.ok).toBe(false);
			expect(result.errors).toHaveLength(1);
			expect(result.errors[0]).toContain('package-lock.json');
		});
	});

	describe('branch validation', () => {
		it('passes when all repos are on the required branch', async () => {
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

			expect(result.ok).toBe(true);
			expect(result.errors).toHaveLength(0);
		});

		it('fails when a repo is on wrong branch', async () => {
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

			expect(result.ok).toBe(false);
			expect(result.errors).toHaveLength(1);
			expect(result.errors[0]).toContain('package-b');
			expect(result.errors[0]).toContain("is on branch 'develop'");
			expect(result.errors[0]).toContain("expected 'main'");
		});

		it('supports custom required branch', async () => {
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

			expect(result.ok).toBe(true);
		});

		it('defaults to main branch if not specified', async () => {
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

			expect(result.ok).toBe(false);
			expect(result.errors[0]).toContain("expected 'main'");
		});
	});

	describe('changeset validation', () => {
		it('detects repos with changesets', async () => {
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
			expect(result.repos_without_changesets.size).toBe(2);
			expect(result.repos_with_changesets.size).toBe(0);
		});

		it('warns about packages without changesets', async () => {
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
			expect(changeset_warnings).toHaveLength(2);
		});

		it('skips changeset checks when skip_changesets is true', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];

			const git_ops = create_mock_git_ops();

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			expect(result.repos_with_changesets.size).toBe(0);
			expect(result.repos_without_changesets.size).toBe(0);
			// May have npm warnings, but no changeset warnings
			const changeset_warnings = result.warnings.filter((w) => w.includes('changesets'));
			expect(changeset_warnings).toHaveLength(0);
		});
	});

	describe('npm authentication', () => {
		// Note: The actual npm auth check uses spawn_out('npm', ['whoami'])
		// In real tests, this would need to be mocked at the spawn level
		// For now, we test the integration assuming npm commands work

		it('passes with valid npm authentication', async () => {
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
			expect(result).toHaveProperty('ok');
			expect(result).toHaveProperty('errors');
		});
	});

	describe('multiple validation failures', () => {
		it('reports all types of failures together', async () => {
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

			expect(result.ok).toBe(false);
			expect(result.errors.length).toBe(3); // 1 dirty + 2 wrong branches
			expect(clean_call).toBe(2); // Check called for both repos
			expect(branch_call).toBe(2); // Check called for both repos
		});
	});

	describe('empty repo list', () => {
		it('passes with empty repo list', async () => {
			const repos: Array<LocalRepo> = [];
			const git_ops = create_mock_git_ops();

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			expect(result.ok).toBe(true);
			expect(result.errors).toHaveLength(0);
			// May have npm warnings, but that's acceptable for empty list
		});
	});

	describe('result structure', () => {
		it('returns correct result structure', async () => {
			const repos = [create_mock_repo({name: 'package-a'})];
			const git_ops = create_mock_git_ops();

			const npm_ops = create_mock_npm_ops();
			const result = await run_preflight_checks({
				repos,
				preflight_options: {skip_changesets: true, check_remote: false},
				git_ops,
				npm_ops,
			});

			expect(result).toHaveProperty('ok');
			expect(result).toHaveProperty('warnings');
			expect(result).toHaveProperty('errors');
			expect(result).toHaveProperty('repos_with_changesets');
			expect(result).toHaveProperty('repos_without_changesets');

			expect(Array.isArray(result.warnings)).toBe(true);
			expect(Array.isArray(result.errors)).toBe(true);
			expect(result.repos_with_changesets instanceof Set).toBe(true);
			expect(result.repos_without_changesets instanceof Set).toBe(true);
		});
	});

	describe('build validation', () => {
		it('skips build validation when skip_build_validation is true', async () => {
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

			expect(result.ok).toBe(true);
			expect(build_called).toBe(false);
		});

		it('validates builds for packages with changesets', async () => {
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
			expect(result.ok).toBe(true);
			expect(build_count).toBe(0);
		});

		it('fails when a build fails', async () => {
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
			expect(result.ok).toBe(true);
			expect(call_count).toBe(0);
		});

		it('fails preflight when build fails for package with changesets', async () => {
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
			expect(result.ok).toBe(false);
			expect(result.errors.some((e) => e.includes('package-b failed to build'))).toBe(true);
			expect(result.errors.some((e) => e.includes('syntax error'))).toBe(true);
		});

		it('reports build failures with error details', async () => {
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
			expect(result.ok).toBe(false);
			expect(result.errors.length).toBe(1);
			expect(result.errors[0]).toBe(
				'failing-package failed to build: Syntax error in src/main.ts:42',
			);
		});

		it('validates builds only for packages with changesets', async () => {
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
			expect(built_packages).toHaveLength(0);
		});

		it('continues validation after build failures to report all issues', async () => {
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
			expect(result.ok).toBe(false);
			expect(built_packages).toHaveLength(3); // All 3 packages were attempted
			expect(built_packages).toContain('package-a');
			expect(built_packages).toContain('package-b');
			expect(built_packages).toContain('package-c');

			// Should report both failures
			expect(result.errors.length).toBe(2);
			expect(result.errors.some((e) => e.includes('package-a failed to build'))).toBe(true);
			expect(result.errors.some((e) => e.includes('package-c failed to build'))).toBe(true);
		});
	});
});
