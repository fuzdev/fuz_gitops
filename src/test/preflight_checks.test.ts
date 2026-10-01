import { assert, describe, test } from 'vitest';

import { run_preflight_checks } from '$lib/preflight_checks.ts';
import type { VersionChange } from '$lib/publishing_plan.ts';
import {
	create_mock_build_ops,
	create_mock_npm_ops,
	create_mock_repo,
	entry_with
} from './test_helpers.ts';
import type { LocalRepo } from '$lib/local_repo.ts';
import type { BuildOperations } from '$lib/operations.ts';

const change = (package_name: string, has_changesets = true): VersionChange => ({
	package_name,
	from: '1.0.0',
	to: '1.0.1',
	bump_type: 'patch',
	breaking: false,
	has_changesets,
	will_generate_changeset: has_changesets ? undefined : true
});

/** Build ops recording each package built, failing those in `failures` with their message. */
const create_recording_build_ops = (
	failures: Record<string, { message: string; output?: string }> = {}
): { build_ops: BuildOperations; built: Array<string> } => {
	const built: Array<string> = [];
	const build_ops = create_mock_build_ops({
		build_package: async ({ repo }) => {
			const name = repo.library.name;
			built.push(name);
			const failure = failures[name];
			return failure ? { ok: false, ...failure } : { ok: true };
		}
	});
	return { build_ops, built };
};

describe('preflight_checks', () => {
	describe('repo git state', () => {
		test("isn't preflight's: a repo off its branch and dirty passes (the readiness gate refuses it)", async () => {
			const not_at_rest: LocalRepo = {
				...create_mock_repo({ name: 'package-a' }),
				entry: entry_with('package-a', {
					checkout: {
						head: { kind: 'branch', name: 'feature' },
						uncommitted: { staged: 0, unstaged: 0, untracked: 1, conflicted: 0 }
					},
					at_rest: {
						on_branch: false,
						clean: false,
						idle: true,
						followed: { kind: 'ahead', commits: 1 }
					}
				})
			};
			const { build_ops } = create_recording_build_ops();

			const result = await run_preflight_checks({
				repos: [not_at_rest],
				version_changes: [change('package-a')],
				npm_ops: create_mock_npm_ops(),
				build_ops
			});

			assert.strictEqual(result.ok, true);
			assert.deepEqual(result.errors, []);
		});
	});

	describe('build validation', () => {
		test("builds exactly the plan's version changes, explicit and auto alike", async () => {
			const repos = [
				create_mock_repo({ name: 'explicit' }),
				create_mock_repo({ name: 'auto' }),
				create_mock_repo({ name: 'unchanged' })
			];
			const { build_ops, built } = create_recording_build_ops();

			const result = await run_preflight_checks({
				repos,
				version_changes: [change('explicit'), change('auto', false)],
				npm_ops: create_mock_npm_ops(),
				build_ops
			});

			assert.strictEqual(result.ok, true);
			assert.deepEqual(built, ['explicit', 'auto']);
			// a repo with nothing to publish is the plan's to report, not preflight's
			assert.deepEqual(result.warnings, []);
		});

		test('builds nothing when the plan publishes nothing', async () => {
			const { build_ops, built } = create_recording_build_ops();

			const result = await run_preflight_checks({
				repos: [create_mock_repo({ name: 'package-a' })],
				version_changes: [],
				npm_ops: create_mock_npm_ops(),
				build_ops
			});

			assert.strictEqual(result.ok, true);
			assert.deepEqual(built, []);
			assert.deepEqual(result.warnings, []);
		});

		test('fails on a planned package missing from the repos', async () => {
			const { build_ops, built } = create_recording_build_ops();

			const result = await run_preflight_checks({
				repos: [create_mock_repo({ name: 'package-a' })],
				version_changes: [change('package-a'), change('ghost')],
				npm_ops: create_mock_npm_ops(),
				build_ops
			});

			assert.strictEqual(result.ok, false);
			assert.deepEqual(built, ['package-a']);
			assert.deepEqual(result.errors, ['ghost is in the plan but not among the repos']);
		});

		test("reports a failed build with the build's output, else its message", async () => {
			const repos = [create_mock_repo({ name: 'with-output' }), create_mock_repo({ name: 'bare' })];
			const { build_ops } = create_recording_build_ops({
				'with-output': { message: 'Build failed', output: 'Syntax error in src/main.ts:42' },
				bare: { message: 'spawn gro ENOENT' }
			});

			const result = await run_preflight_checks({
				repos,
				version_changes: [change('with-output'), change('bare')],
				npm_ops: create_mock_npm_ops(),
				build_ops
			});

			assert.strictEqual(result.ok, false);
			assert.deepEqual(result.errors, [
				'with-output failed to build: Syntax error in src/main.ts:42',
				'bare failed to build: spawn gro ENOENT'
			]);
		});

		test('continues after a failed build to report every failure', async () => {
			const repos = [
				create_mock_repo({ name: 'package-a' }),
				create_mock_repo({ name: 'package-b' }),
				create_mock_repo({ name: 'package-c' })
			];
			const { build_ops, built } = create_recording_build_ops({
				'package-a': { message: 'Build error' },
				'package-c': { message: 'Build error' }
			});

			const result = await run_preflight_checks({
				repos,
				version_changes: [change('package-a'), change('package-b'), change('package-c')],
				npm_ops: create_mock_npm_ops(),
				build_ops
			});

			assert.strictEqual(result.ok, false);
			assert.deepEqual(built, ['package-a', 'package-b', 'package-c']);
			assert.deepEqual(result.errors, [
				'package-a failed to build: Build error',
				'package-c failed to build: Build error'
			]);
		});
	});

	describe('npm', () => {
		test('fails when npm authentication fails', async () => {
			const result = await run_preflight_checks({
				repos: [],
				version_changes: [],
				npm_ops: create_mock_npm_ops({
					check_auth: async () => ({ ok: false, message: 'E401' })
				})
			});

			assert.strictEqual(result.ok, false);
			assert.deepEqual(result.errors, ['npm authentication failed: E401']);
		});

		test('warns when the registry is unreachable', async () => {
			const result = await run_preflight_checks({
				repos: [],
				version_changes: [],
				npm_ops: create_mock_npm_ops({
					check_registry: async () => ({ ok: false, message: 'ping failed' })
				})
			});

			assert.strictEqual(result.ok, true);
			assert.deepEqual(result.warnings, ['npm registry check failed: ping failed']);
		});

		test('passes with an empty plan and npm ready', async () => {
			const result = await run_preflight_checks({
				repos: [],
				version_changes: [],
				npm_ops: create_mock_npm_ops()
			});

			assert.deepEqual(result, { ok: true, warnings: [], errors: [] });
		});
	});
});
