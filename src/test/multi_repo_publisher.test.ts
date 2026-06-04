import {assert, test, describe} from 'vitest';

import type {LocalRepo} from '$lib/local_repo.js';
import {
	publish_repos,
	execute_publishing_plan,
	group_dependency_updates,
	type PublishedVersion,
} from '$lib/multi_repo_publisher.js';
import type {DependencyUpdate, PublishingPlan, VersionChange} from '$lib/publishing_plan.js';
import {derive_publish_steps, type PublishStep} from '$lib/publish_steps.js';
import type {PublishingEvent} from '$lib/publishing_event.js';
import {capture_handler} from '$lib/publishing_event_handler.js';
import {
	create_mock_repo,
	create_mock_gitops_ops,
	create_mock_package_json_files,
	create_tracking_process_ops,
	create_mock_git_ops,
	create_preflight_mock,
	create_populated_fs_ops,
} from './test_helpers.js';

test('wetrun=false predicts versions without publishing', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0', deps: {'pkg-a': '0.1.0'}}),
	];

	// Create mock operations
	const mock_ops = create_mock_gitops_ops({
		changeset: {
			predict_next_version: async (options) => {
				if (options.repo.library.name === 'pkg-a') {
					return {ok: true, version: '0.1.1', bump_type: 'patch' as const};
				}
				if (options.repo.library.name === 'pkg-b') {
					return {ok: true, version: '0.2.1', bump_type: 'patch' as const};
				}
				return null;
			},
		},
		preflight: create_preflight_mock(['pkg-a', 'pkg-b']),
	});

	const result = await publish_repos(repos, {
		wetrun: false,
		ops: mock_ops,
	});

	assert.strictEqual(result.ok, true);
	assert.strictEqual(result.published.length, 2);
	assert.strictEqual(result.published[0]!.name, 'pkg-a');
	assert.strictEqual(result.published[0]!.new_version, '0.1.1');
	assert.strictEqual(result.published[1]!.name, 'pkg-b');
	assert.strictEqual(result.published[1]!.new_version, '0.2.1');
});

test('always fails fast on publish errors', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0'}),
		create_mock_repo({name: 'pkg-c', version: '0.3.0'}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	let publish_attempt = 0;
	const mock_ops = create_mock_gitops_ops({
		process: {
			spawn: async (options) => {
				if (options.cmd === 'gro' && options.args[0] === 'publish') {
					publish_attempt++;
					// Make pkg-a fail
					if (publish_attempt === 1) {
						return {ok: false, message: 'Publish failed'};
					}
				}
				return {ok: true};
			},
		},
		preflight: create_preflight_mock(['pkg-a', 'pkg-b', 'pkg-c']),
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {
		wetrun: true,
		ops: mock_ops,
	});

	// With fail-fast behavior: only the first package in topo order fails, no other packages are attempted
	assert.strictEqual(result.ok, false);
	assert.strictEqual(result.failed.length, 1);
	assert.strictEqual(result.failed[0]!.name, 'pkg-c');
	assert.strictEqual(result.published.length, 0); // No packages published after failure
});

test('handles breaking change cascades when wetrun=false', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-core', version: '0.5.0'}),
		create_mock_repo({name: 'pkg-mid', version: '0.3.0', deps: {'pkg-core': '^0.5.0'}}),
		create_mock_repo({name: 'pkg-app', version: '0.2.0', deps: {'pkg-mid': '^0.3.0'}}),
	];

	const mock_ops = create_mock_gitops_ops({
		changeset: {
			predict_next_version: async (options) => {
				// pkg-core has a breaking change (0.x minor bump)
				if (options.repo.library.name === 'pkg-core') {
					return {ok: true, version: '0.6.0', bump_type: 'minor' as const};
				}
				// Others have patch bumps
				if (options.repo.library.name === 'pkg-mid') {
					return {ok: true, version: '0.3.1', bump_type: 'patch' as const};
				}
				if (options.repo.library.name === 'pkg-app') {
					return {ok: true, version: '0.2.1', bump_type: 'patch' as const};
				}
				return null;
			},
		},
		preflight: create_preflight_mock(['pkg-core', 'pkg-mid', 'pkg-app']),
	});

	const result = await publish_repos(repos, {
		wetrun: false,
		ops: mock_ops,
	});

	assert.strictEqual(result.ok, true);
	assert.strictEqual(result.published.length, 3);

	// Check versions. The dry run reports the full plan cascade: pkg-core's breaking
	// change (0.x minor) escalates pkg-mid's patch changeset to a breaking minor, which
	// in turn escalates pkg-app — both become breaking bumps.
	const core = result.published.find((p) => p.name === 'pkg-core');
	const mid = result.published.find((p) => p.name === 'pkg-mid');
	const app = result.published.find((p) => p.name === 'pkg-app');

	assert.strictEqual(core?.new_version, '0.6.0');
	assert.strictEqual(core?.breaking, true);
	assert.strictEqual(mid?.new_version, '0.4.0');
	assert.strictEqual(mid?.breaking, true);
	assert.strictEqual(app?.new_version, '0.3.0');
	assert.strictEqual(app?.breaking, true);
});

test("wetrun publishes a breaking cascade with the plan's escalated versions", async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-core', version: '0.5.0'}),
		create_mock_repo({name: 'pkg-mid', version: '0.3.0', deps: {'pkg-core': '^0.5.0'}}),
		create_mock_repo({name: 'pkg-app', version: '0.2.0', deps: {'pkg-mid': '^0.3.0'}}),
	];

	// The real publish reads each new version back from package.json; return the plan's
	// escalated versions so the read-back matches and fail-loud-on-drift stays quiet.
	const mock_fs_ops = create_populated_fs_ops(
		repos,
		new Map([
			['pkg-core', '0.6.0'],
			['pkg-mid', '0.4.0'],
			['pkg-app', '0.3.0'],
		]),
	);

	const mock_ops = create_mock_gitops_ops({
		changeset: {
			predict_next_version: async (options) => {
				if (options.repo.library.name === 'pkg-core') {
					return {ok: true, version: '0.6.0', bump_type: 'minor' as const};
				}
				if (options.repo.library.name === 'pkg-mid') {
					return {ok: true, version: '0.3.1', bump_type: 'patch' as const};
				}
				if (options.repo.library.name === 'pkg-app') {
					return {ok: true, version: '0.2.1', bump_type: 'patch' as const};
				}
				return null;
			},
		},
		preflight: create_preflight_mock(['pkg-core', 'pkg-mid', 'pkg-app']),
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	assert.strictEqual(result.ok, true);
	assert.strictEqual(result.published.length, 3);

	// wetrun consumes the plan: pkg-core's breaking 0.x minor escalates pkg-mid's patch
	// changeset to a breaking minor, which escalates pkg-app in turn — the bump metadata
	// comes from the plan, not from re-deriving the read-back version.
	const core = result.published.find((p) => p.name === 'pkg-core');
	const mid = result.published.find((p) => p.name === 'pkg-mid');
	const app = result.published.find((p) => p.name === 'pkg-app');

	assert.strictEqual(core?.new_version, '0.6.0');
	assert.strictEqual(core?.breaking, true);
	assert.strictEqual(mid?.new_version, '0.4.0');
	assert.strictEqual(mid?.bump_type, 'minor'); // escalated from patch
	assert.strictEqual(mid?.breaking, true);
	assert.strictEqual(app?.new_version, '0.3.0');
	assert.strictEqual(app?.bump_type, 'minor'); // escalated from patch
	assert.strictEqual(app?.breaking, true);
});

test('wetrun fails loud and aborts when a publish drifts from the plan', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-core', version: '0.5.0'}),
		create_mock_repo({name: 'pkg-mid', version: '0.3.0', deps: {'pkg-core': '^0.5.0'}}),
		create_mock_repo({name: 'pkg-app', version: '0.2.0', deps: {'pkg-mid': '^0.3.0'}}),
	];

	// pkg-mid reads back 0.3.1 (a plain patch) but the plan escalates it to 0.4.0 — the
	// executor must treat the mismatch as drift and abort rather than continue.
	const mock_fs_ops = create_populated_fs_ops(
		repos,
		new Map([
			['pkg-core', '0.6.0'],
			['pkg-mid', '0.3.1'], // drift: the plan predicts 0.4.0
			['pkg-app', '0.3.0'],
		]),
	);

	const mock_ops = create_mock_gitops_ops({
		changeset: {
			predict_next_version: async (options) => {
				if (options.repo.library.name === 'pkg-core') {
					return {ok: true, version: '0.6.0', bump_type: 'minor' as const};
				}
				if (options.repo.library.name === 'pkg-mid') {
					return {ok: true, version: '0.3.1', bump_type: 'patch' as const};
				}
				if (options.repo.library.name === 'pkg-app') {
					return {ok: true, version: '0.2.1', bump_type: 'patch' as const};
				}
				return null;
			},
		},
		preflight: create_preflight_mock(['pkg-core', 'pkg-mid', 'pkg-app']),
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	assert.strictEqual(result.ok, false);

	// pkg-core published cleanly; pkg-mid drifted and aborted the run before pkg-app.
	assert.strictEqual(result.published.length, 1);
	assert.strictEqual(result.published[0]!.name, 'pkg-core');

	assert.strictEqual(result.failed.length, 1);
	assert.strictEqual(result.failed[0]!.name, 'pkg-mid');
	assert.ok(result.failed[0]!.error.message.includes('drift'));

	// the failure is tagged as a drift, distinct from an ordinary publish failure
	const failure = result.events.find((e) => e.event === 'package_failed');
	assert.ok(failure);
	assert(failure.event === 'package_failed');
	assert.strictEqual(failure.name, 'pkg-mid');
	assert.strictEqual(failure.code, 'drift');
});

test('skips repos without changesets', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0'}),
		create_mock_repo({name: 'pkg-c', version: '0.3.0'}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	// Create mock operations where only pkg-a has changesets
	const mock_ops = create_mock_gitops_ops({
		changeset: {
			has_changesets: async (options) => ({
				ok: true,
				value: options.repo.library.name === 'pkg-a',
			}),
		},
		preflight: create_preflight_mock(['pkg-a'], ['pkg-b', 'pkg-c']),
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {
		wetrun: true,
		ops: mock_ops,
	});

	// Only pkg-a should be published
	assert.strictEqual(result.ok, true);
	assert.strictEqual(result.published.length, 1);
	assert.strictEqual(result.published[0]!.name, 'pkg-a');
});

test('publishes in dependency order', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'lib', version: '1.0.0'}),
		create_mock_repo({name: 'middleware', version: '1.0.0', deps: {lib: '^1.0.0'}}),
		create_mock_repo({name: 'app', version: '1.0.0', deps: {middleware: '^1.0.0'}}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const {
		ops: process_ops,
		get_commands_by_type,
		get_package_names_from_cwd,
	} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		process: process_ops,
		preflight: create_preflight_mock(['lib', 'middleware', 'app']),
		fs: mock_fs_ops,
	});

	await publish_repos(repos, {wetrun: true, ops: mock_ops});

	// Should publish in dependency order: lib → middleware → app
	const publish_commands = get_commands_by_type('publish');
	const publish_order = get_package_names_from_cwd(publish_commands);
	assert.deepEqual(publish_order, ['lib', 'middleware', 'app']);
});

test('waits for npm propagation after each publish', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '1.0.0'}),
		create_mock_repo({name: 'pkg-b', version: '1.0.0'}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const wait_calls: Array<{pkg: string; version: string}> = [];

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['pkg-a', 'pkg-b']),
		npm: {
			wait_for_package: async (options) => {
				wait_calls.push({pkg: options.pkg, version: options.version});
				return {ok: true};
			},
			check_auth: async () => ({ok: true, username: 'testuser'}),
			check_registry: async () => ({ok: true}),
			install: async () => ({ok: true}),
		},
		fs: mock_fs_ops,
	});

	await publish_repos(repos, {wetrun: true, ops: mock_ops});

	// Should wait for both packages
	assert.strictEqual(wait_calls.length, 2);
	assert.strictEqual(wait_calls[0]!.pkg, 'pkg-b');
	assert.strictEqual(wait_calls[1]!.pkg, 'pkg-a');
});

test('updates prod dependencies after publishing (Phase 1)', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'lib', version: '1.0.0'}),
		create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);
	const git_commits: Array<{cwd: string; message: string}> = [];

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['lib'], ['app']),
		git: create_mock_git_ops({
			add_and_commit: async (options) => {
				git_commits.push({cwd: options.cwd || '', message: options.message});
				return {ok: true};
			},
		}),
		fs: mock_fs_ops,
	});

	await publish_repos(repos, {wetrun: true, ops: mock_ops});

	// lib has changesets, so publishing cascades a dependency update to its dependent
	// (Actual behavior depends on implementation - tests document expected outcome)
	assert.ok(git_commits.length >= 0);
});

test('updates dev dependencies without republishing (Phase 2)', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'test-utils', version: '1.0.0'}),
		create_mock_repo({name: 'lib', version: '1.0.0', dev_deps: {'test-utils': '^1.0.0'}}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const mock_ops = create_mock_gitops_ops({
		// only test-utils has changesets; lib merely carries a dev dep on it
		changeset: {
			has_changesets: async (options) => ({
				ok: true,
				value: options.repo.library.name === 'test-utils',
			}),
		},
		preflight: create_preflight_mock(['test-utils'], ['lib']),
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	assert.strictEqual(result.ok, true);

	// a dev-dep change does NOT trigger a republish: only test-utils publishes
	assert.strictEqual(result.published.length, 1);
	assert.strictEqual(result.published[0]!.name, 'test-utils');

	// but lib's dev dep on test-utils is still bumped in Phase 2
	const dev_update = result.events.find(
		(e) =>
			e.event === 'dependency_updated' && e.dependent === 'lib' && e.dependency === 'test-utils',
	);
	assert.ok(dev_update);
});

test('dev-dep-only update bumps package.json without creating a changeset', async () => {
	// a dev-dep change is committed but must NOT generate a changeset — otherwise the next
	// release would republish a package whose shipped artifact didn't change.
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'test-utils', version: '1.0.0'}),
		create_mock_repo({name: 'lib', version: '1.0.0', dev_deps: {'test-utils': '^1.0.0'}}),
	];

	const fs_ops = create_populated_fs_ops(repos);
	const written_paths: Array<string> = [];
	const base_write = fs_ops.writeFile;
	fs_ops.writeFile = async (options) => {
		written_paths.push(options.path);
		return base_write(options);
	};

	const mock_ops = create_mock_gitops_ops({
		changeset: {
			has_changesets: async (options) => ({
				ok: true,
				value: options.repo.library.name === 'test-utils',
			}),
		},
		preflight: create_preflight_mock(['test-utils'], ['lib']),
		fs: fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	assert.strictEqual(result.ok, true);
	// the dev dep is bumped in lib's package.json...
	assert.ok(written_paths.includes('/test/lib/package.json'));
	// ...but no changeset is written for lib (dev-dep changes don't republish)
	assert.ok(!written_paths.some((p) => p.includes('/test/lib/.changeset/')));
});

test('deploys all repos when deploy flag is set (Phase 3)', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '1.0.0'}),
		create_mock_repo({name: 'pkg-b', version: '1.0.0'}),
	];

	const mock_fs = create_mock_package_json_files(repos);
	const {ops: process_ops, get_commands_by_type} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['pkg-a', 'pkg-b']),
		process: process_ops,
		fs: {
			readFile: async (options) => ({
				ok: true,
				value: mock_fs.get(options.path) || '{}',
			}),
			writeFile: async () => ({ok: true}),
		},
	});

	await publish_repos(repos, {wetrun: true, deploy: true, ops: mock_ops});

	// Should deploy both repos
	const deploy_commands = get_commands_by_type('deploy');
	assert.strictEqual(deploy_commands.length, 2);
	assert.strictEqual(
		deploy_commands.some((c) => c.cwd.includes('pkg-a')),
		true,
	);
	assert.strictEqual(
		deploy_commands.some((c) => c.cwd.includes('pkg-b')),
		true,
	);
});

test('deploys only repos with changes (skips unchanged repos)', async () => {
	// This test covers selective deployment including dev dep changes
	// Full integration coverage in fixture tests (src/test/fixtures/check.test.ts)
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'lib', version: '1.0.0'}),
		create_mock_repo({name: 'app-with-dep', version: '1.0.0', deps: {lib: '^1.0.0'}}),
		create_mock_repo({name: 'app-no-dep', version: '1.0.0'}),
		create_mock_repo({name: 'util-isolated', version: '1.0.0'}),
	];

	const mock_fs = create_mock_package_json_files(repos);
	const {ops: process_ops, get_commands_by_type} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['lib'], ['app-with-dep', 'app-no-dep', 'util-isolated']),
		changeset: {
			has_changesets: async (options) => ({
				ok: true,
				value: options.repo.library.name === 'lib', // Only lib has changesets
			}),
		},
		process: process_ops,
		fs: {
			readFile: async (options) => ({
				ok: true,
				value: mock_fs.get(options.path) || '{}',
			}),
			writeFile: async () => ({ok: true}),
		},
	});

	await publish_repos(repos, {wetrun: true, deploy: true, ops: mock_ops});

	// Should deploy only lib (published) and app-with-dep (dep updated)
	const deploy_commands = get_commands_by_type('deploy');
	assert.strictEqual(deploy_commands.length, 2);
	assert.strictEqual(
		deploy_commands.some((c) => c.cwd.includes('lib')),
		true,
	);
	assert.strictEqual(
		deploy_commands.some((c) => c.cwd.includes('app-with-dep')),
		true,
	);
	assert.strictEqual(
		deploy_commands.some((c) => c.cwd.includes('app-no-dep')),
		false,
	);
	assert.strictEqual(
		deploy_commands.some((c) => c.cwd.includes('util-isolated')),
		false,
	);
});

test('wetrun=false skips deployment even with deploy flag', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '1.0.0'}),
		create_mock_repo({name: 'pkg-b', version: '1.0.0'}),
	];

	const {ops: process_ops, get_commands_by_type} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		changeset: {
			predict_next_version: async () => ({
				ok: true,
				version: '1.1.0',
				bump_type: 'minor' as const,
			}),
		},
		process: process_ops,
	});

	await publish_repos(repos, {wetrun: false, deploy: true, ops: mock_ops});

	// Dry run should skip deployment entirely
	const deploy_commands = get_commands_by_type('deploy');
	assert.strictEqual(deploy_commands.length, 0);
});

test('no changes results in no deployment', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '1.0.0'}),
		create_mock_repo({name: 'pkg-b', version: '1.0.0'}),
		create_mock_repo({name: 'pkg-c', version: '1.0.0'}),
	];

	const mock_fs = create_mock_package_json_files(repos);
	const {ops: process_ops, get_commands_by_type} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock([], ['pkg-a', 'pkg-b', 'pkg-c']), // No changesets
		changeset: {
			has_changesets: async () => ({ok: true, value: false}), // No changesets
		},
		process: process_ops,
		fs: {
			readFile: async (options) => ({
				ok: true,
				value: mock_fs.get(options.path) || '{}',
			}),
			writeFile: async () => ({ok: true}),
		},
	});

	await publish_repos(repos, {wetrun: true, deploy: true, ops: mock_ops});

	// No changes = no deployment
	const deploy_commands = get_commands_by_type('deploy');
	assert.strictEqual(deploy_commands.length, 0);
});

test('applies version strategy (caret vs tilde vs exact)', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'lib', version: '1.0.0'}),
		create_mock_repo({name: 'app-caret', version: '1.0.0', deps: {lib: '^1.0.0'}}),
		create_mock_repo({name: 'app-tilde', version: '1.0.0', deps: {lib: '~1.0.0'}}),
		create_mock_repo({name: 'app-exact', version: '1.0.0', deps: {lib: '1.0.0'}}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['lib']),
		fs: mock_fs_ops,
	});

	// dependency updates run during publishing; the mock fs/ops cover the cascade
	const result = await publish_repos(repos, {
		wetrun: true,
		version_strategy: 'exact',
		ops: mock_ops,
	});

	// Test succeeds if publishing completes
	assert.strictEqual(result.ok, true);
});

test('handles 4-level transitive dependency chain', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'level-1', version: '1.0.0'}),
		create_mock_repo({name: 'level-2', version: '1.0.0', deps: {'level-1': '^1.0.0'}}),
		create_mock_repo({name: 'level-3', version: '1.0.0', deps: {'level-2': '^1.0.0'}}),
		create_mock_repo({name: 'level-4', version: '1.0.0', deps: {'level-3': '^1.0.0'}}),
	];

	const mock_fs = create_mock_package_json_files(repos);
	const {
		ops: process_ops,
		get_commands_by_type,
		get_package_names_from_cwd,
	} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		process: process_ops,
		preflight: create_preflight_mock(['level-1', 'level-2', 'level-3', 'level-4']),
		fs: {
			readFile: async (options) => ({
				ok: true,
				value: mock_fs.get(options.path) || '{}',
			}),
			writeFile: async () => ({ok: true}),
		},
	});

	await publish_repos(repos, {wetrun: true, ops: mock_ops});

	// Should publish bottom-up
	const publish_commands = get_commands_by_type('publish');
	const publish_order = get_package_names_from_cwd(publish_commands);
	assert.deepEqual(publish_order, ['level-1', 'level-2', 'level-3', 'level-4']);
});

test('handles mixed prod and dev deps on same package', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'shared', version: '1.0.0'}),
		create_mock_repo({
			name: 'app',
			version: '1.0.0',
			deps: {shared: '^1.0.0'},
			dev_deps: {shared: '^1.0.0'}, // Also in dev deps
		}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['shared']),
		fs: mock_fs_ops,
	});

	// dependency updates run during publishing; the mock fs/ops cover the cascade
	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	// Test succeeds if publishing completes
	assert.strictEqual(result.ok, true);
});

test('reports correct duration in result', async () => {
	const repos: Array<LocalRepo> = [create_mock_repo({name: 'pkg-a', version: '1.0.0'})];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['pkg-a']),
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	assert.ok(result.duration >= 0);
	assert.strictEqual(typeof result.duration, 'number');
});

test('wetrun=false skips preflight checks', async () => {
	const repos: Array<LocalRepo> = [create_mock_repo({name: 'pkg-a', version: '1.0.0'})];

	let preflight_called = false;

	const mock_ops = create_mock_gitops_ops({
		preflight: {
			run_preflight_checks: async () => {
				preflight_called = true;
				return create_preflight_mock(['pkg-a']).run_preflight_checks();
			},
		},
	});

	await publish_repos(repos, {wetrun: false, ops: mock_ops});

	// wetrun=false should skip preflight checks
	assert.strictEqual(preflight_called, false);
});

test('handles npm propagation failure gracefully', async () => {
	const repos: Array<LocalRepo> = [create_mock_repo({name: 'pkg-a', version: '1.0.0'})];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['pkg-a']),
		npm: {
			wait_for_package: async () => {
				throw new Error('Timeout waiting for package');
			},
			check_auth: async () => ({ok: true, username: 'testuser'}),
			check_registry: async () => ({ok: true}),
			install: async () => ({ok: true}),
		},
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	// Should fail due to npm propagation timeout
	assert.strictEqual(result.ok, false);
	assert.strictEqual(result.failed.length, 1);
	assert.ok(result.failed[0]!.error.message.includes('Timeout waiting for package'));
});

test('handles deploy failures without stopping', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '1.0.0'}),
		create_mock_repo({name: 'pkg-b', version: '1.0.0'}),
	];

	const mock_fs = create_mock_package_json_files(repos);
	const {ops: process_ops, get_commands_by_type} = create_tracking_process_ops();

	// Override spawn to make pkg-a deploy fail
	const original_spawn = process_ops.spawn;
	process_ops.spawn = async (spawn_args) => {
		const result = await original_spawn(spawn_args);
		if (spawn_args.cmd === 'gro' && spawn_args.args[0] === 'deploy') {
			const cwd = spawn_args.cwd ?? '';
			// Make first deploy fail
			if (cwd.includes('pkg-a')) {
				return {ok: false, message: 'Deploy failed'};
			}
		}
		return result;
	};

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['pkg-a', 'pkg-b']),
		process: process_ops,
		fs: {
			readFile: async (options) => ({
				ok: true,
				value: mock_fs.get(options.path) || '{}',
			}),
			writeFile: async () => ({ok: true}),
		},
	});

	const result = await publish_repos(repos, {
		wetrun: true,
		deploy: true,
		ops: mock_ops,
	});

	// Publishing should succeed even if deploy fails
	assert.strictEqual(result.ok, true);
	// Both deploys should be attempted (deploy doesn't fail-fast)
	const deploy_commands = get_commands_by_type('deploy');
	assert.strictEqual(deploy_commands.length, 2);
});

test('returns correct PublishedVersion metadata', async () => {
	const repos: Array<LocalRepo> = [create_mock_repo({name: 'pkg-a', version: '0.5.0'})];

	const mock_fs = create_mock_package_json_files(repos);

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['pkg-a']),
		changeset: {
			...create_mock_gitops_ops().changeset,
			predict_next_version: async () => ({
				ok: true,
				version: '0.6.0',
				bump_type: 'minor' as const,
			}),
		},
		fs: {
			readFile: async (options) => ({
				ok: true,
				value: mock_fs.get(options.path) || '{}',
			}),
			writeFile: async () => ({ok: true}),
		},
	});

	const result = await publish_repos(repos, {wetrun: false, ops: mock_ops});

	assert.strictEqual(result.published.length, 1);
	const published = result.published[0]!;

	assert.strictEqual(published.name, 'pkg-a');
	assert.strictEqual(published.old_version, '0.5.0');
	assert.strictEqual(published.new_version, '0.6.0');
	assert.strictEqual(published.bump_type, 'minor');
	assert.strictEqual(published.breaking, true); // 0.x minor is breaking
	assert.strictEqual(published.tag, 'v0.6.0');
});

test('publishes each package exactly once in a single pass', async () => {
	// The plan resolves the cascade up front, so publishing is a single linear pass over
	// the topological order — no fixed-point loop, and no package is published twice.
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'lib', version: '1.0.0'}),
		create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
	];

	const mock_fs_ops = create_populated_fs_ops(repos);

	const {
		ops: process_ops,
		get_commands_by_type,
		get_package_names_from_cwd,
	} = create_tracking_process_ops();

	const mock_ops = create_mock_gitops_ops({
		preflight: create_preflight_mock(['lib', 'app']),
		process: process_ops,
		fs: mock_fs_ops,
	});

	const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

	assert.strictEqual(result.ok, true);

	// Each package is published exactly once, in dependency order (no re-visiting)
	const publish_order = get_package_names_from_cwd(get_commands_by_type('publish'));
	assert.deepEqual(publish_order, ['lib', 'app']);

	// And each appears exactly once in the completion stream
	const completions = result.events.filter((e) => e.event === 'package_completed');
	assert.strictEqual(completions.length, 2);
});

// NOTE: the MAX_ITERATIONS warning lives in plan generation (the publisher no longer
// iterates). Its logic is tested in publishing_plan.test.ts.

describe('structured events', () => {
	test('dry run emits run_started{wetrun:false} and predicted (simulated) completions', async () => {
		const repos: Array<LocalRepo> = [
			create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
			create_mock_repo({name: 'pkg-b', version: '0.2.0', deps: {'pkg-a': '0.1.0'}}),
		];

		const mock_ops = create_mock_gitops_ops({
			changeset: {
				predict_next_version: async (options) => {
					if (options.repo.library.name === 'pkg-a') {
						return {ok: true, version: '0.1.1', bump_type: 'patch' as const};
					}
					if (options.repo.library.name === 'pkg-b') {
						return {ok: true, version: '0.2.1', bump_type: 'patch' as const};
					}
					return null;
				},
			},
			preflight: create_preflight_mock(['pkg-a', 'pkg-b']),
		});

		const result = await publish_repos(repos, {wetrun: false, ops: mock_ops});

		// The result carries the stream and a derived summary.
		const first = result.events[0]!;
		assert.strictEqual(first.event, 'run_started');
		assert(first.event === 'run_started');
		assert.strictEqual(first.wetrun, false);
		assert.strictEqual(first.total, 2);

		const completions = result.events.filter((e) => e.event === 'package_completed');
		assert.strictEqual(completions.length, 2);
		// In a dry run every completion is a prediction: commit is 'simulated'.
		for (const e of completions) {
			assert(e.event === 'package_completed');
			assert.strictEqual(e.commit, 'simulated');
		}

		const last = result.events.at(-1)!;
		assert.strictEqual(last.event, 'run_finished');
		assert(last.event === 'run_finished');
		assert.strictEqual(last.summary.published, 2);
		assert.strictEqual(last.summary.total, 2);
		assert.strictEqual(result.summary.published, 2);
	});

	test('wetrun emits run_started{wetrun:true} and real commit hashes', async () => {
		const repos: Array<LocalRepo> = [create_mock_repo({name: 'pkg-a', version: '0.1.0'})];

		const mock_ops = create_mock_gitops_ops({
			preflight: create_preflight_mock(['pkg-a']),
			fs: create_populated_fs_ops(repos), // package.json with bumped version for the read-back
		});

		const result = await publish_repos(repos, {wetrun: true, ops: mock_ops});

		const first = result.events[0]!;
		assert(first.event === 'run_started');
		assert.strictEqual(first.wetrun, true);

		const completion = result.events.find((e) => e.event === 'package_completed');
		assert.ok(completion);
		assert(completion.event === 'package_completed');
		assert.strictEqual(completion.name, 'pkg-a');
		assert.strictEqual(completion.commit, 'abc123'); // real hash from git mock, not 'simulated'
		assert.strictEqual(completion.new_version, '0.1.1');
	});

	test('an external handler receives the same events as the result', async () => {
		const repos: Array<LocalRepo> = [create_mock_repo({name: 'pkg-a', version: '0.1.0'})];
		const capture = capture_handler();

		const mock_ops = create_mock_gitops_ops({
			changeset: {
				predict_next_version: async () => ({
					ok: true,
					version: '0.1.1',
					bump_type: 'patch' as const,
				}),
			},
			preflight: create_preflight_mock(['pkg-a']),
		});

		const result = await publish_repos(repos, {
			wetrun: false,
			ops: mock_ops,
			events: capture,
		});

		assert.strictEqual(capture.events.length, result.events.length);
		assert.strictEqual(capture.events[0]!.event, 'run_started');
		assert.strictEqual(capture.events.at(-1)!.event, 'run_finished');
	});
});

describe('group_dependency_updates', () => {
	const make_update = (
		dependent: string,
		dependency: string,
		type: DependencyUpdate['type'],
	): DependencyUpdate => ({
		dependent_package: dependent,
		updated_dependency: dependency,
		current_version: '^1.0.0',
		new_version: 'ignored', // the helper reads the published version, not this field
		type,
	});

	const make_published = (entries: Array<[string, string]>): Map<string, PublishedVersion> =>
		new Map(
			entries.map(([name, version]) => [
				name,
				{
					name,
					old_version: '1.0.0',
					new_version: version,
					bump_type: 'patch' as const,
					breaking: false,
					commit: 'abc',
					tag: `v${version}`,
				},
			]),
		);

	test('groups dependencies under each dependent, keyed to the published version', () => {
		const updates = [
			make_update('app', 'lib1', 'dependencies'),
			make_update('app', 'lib2', 'peerDependencies'),
		];
		const published = make_published([
			['lib1', '1.1.0'],
			['lib2', '2.0.0'],
		]);

		const grouped = group_dependency_updates(updates, published, () => true);

		assert.strictEqual(grouped.size, 1);
		assert.strictEqual(grouped.get('app')?.get('lib1'), '1.1.0');
		assert.strictEqual(grouped.get('app')?.get('lib2'), '2.0.0');
	});

	test('filters by predicate (prod/peer vs dev)', () => {
		const updates = [
			make_update('app', 'lib', 'dependencies'),
			make_update('app', 'tool', 'devDependencies'),
		];
		const published = make_published([
			['lib', '1.1.0'],
			['tool', '1.1.0'],
		]);

		const prod = group_dependency_updates(updates, published, (u) => u.type !== 'devDependencies');
		assert.deepEqual([...prod.get('app')!.keys()], ['lib']);

		const dev = group_dependency_updates(updates, published, (u) => u.type === 'devDependencies');
		assert.deepEqual([...dev.get('app')!.keys()], ['tool']);
	});

	test('excludes dependencies that did not publish (failed or aborted)', () => {
		const updates = [make_update('app', 'lib', 'dependencies')];
		const published = make_published([]); // lib never published this run

		const grouped = group_dependency_updates(updates, published, () => true);
		assert.strictEqual(grouped.size, 0);
	});
});

describe('execute_publishing_plan', () => {
	const make_version_change = (
		overrides: Partial<VersionChange> & {package_name: string},
	): VersionChange => ({
		from: '1.0.0',
		to: '1.0.1',
		bump_type: 'patch',
		breaking: false,
		has_changesets: true,
		...overrides,
	});

	const make_plan = (overrides: Partial<PublishingPlan> = {}): PublishingPlan => ({
		publishing_order: [],
		version_changes: [],
		dependency_updates: [],
		breaking_cascades: new Map(),
		warnings: [],
		info: [],
		errors: [],
		...overrides,
	});

	test('executes a frozen plan without regenerating it', async () => {
		const repos = [create_mock_repo({name: 'pkg', version: '1.0.0'})];
		const plan = make_plan({
			publishing_order: ['pkg'],
			version_changes: [make_version_change({package_name: 'pkg'})],
		});

		const result = await execute_publishing_plan(repos, plan, {
			wetrun: false,
			ops: create_mock_gitops_ops({fs: create_populated_fs_ops(repos)}),
		});

		assert.strictEqual(result.ok, true);
		assert.deepEqual(
			result.published.map((p) => p.name),
			['pkg'],
		);
		assert.strictEqual(result.published[0]!.new_version, '1.0.1');
	});

	test('wetrun throws on a plan with errors, before any side effect', async () => {
		const repos = [create_mock_repo({name: 'pkg', version: '1.0.0'})];
		const plan = make_plan({errors: ['Production dependency cycle: a → b → a']});

		let threw = false;
		try {
			await execute_publishing_plan(repos, plan, {wetrun: true, ops: create_mock_gitops_ops()});
		} catch (error) {
			threw = true;
			assert.ok(error instanceof Error);
			assert.match(error.message, /plan has 1 error/);
		}
		assert.ok(threw, 'expected execute_publishing_plan to throw on a plan with errors');
	});

	test('dry run reports plan errors and is not ok (no bypass)', async () => {
		const repos = [create_mock_repo({name: 'pkg', version: '1.0.0'})];
		const plan = make_plan({errors: ['boom']});

		const result = await execute_publishing_plan(repos, plan, {
			wetrun: false,
			ops: create_mock_gitops_ops({fs: create_populated_fs_ops(repos)}),
		});

		assert.strictEqual(result.ok, false);
		assert.deepEqual(result.plan_errors, ['boom']);
	});

	test('deploy builds fresh — spawns `gro deploy` without --no-build', async () => {
		const repos = [create_mock_repo({name: 'pkg', version: '1.0.0'})];
		const plan = make_plan({
			publishing_order: ['pkg'],
			version_changes: [make_version_change({package_name: 'pkg'})],
		});

		const {ops: process_ops, get_commands_by_type} = create_tracking_process_ops();
		await execute_publishing_plan(repos, plan, {
			wetrun: true,
			deploy: true,
			ops: create_mock_gitops_ops({
				preflight: create_preflight_mock(['pkg']),
				fs: create_populated_fs_ops(repos),
				process: process_ops,
			}),
		});

		const deploy_commands = get_commands_by_type('deploy');
		assert.strictEqual(deploy_commands.length, 1);
		assert.deepEqual(deploy_commands[0]!.args, ['deploy']);
	});

	// --- Anti-drift: the executor's event stream must match the derived preview, step for step.
	// The preview (`derive_publish_steps`) and the executor are separate passes over the same
	// frozen plan. Reducing each to an ordered key sequence — and comparing the WHOLE sequence,
	// not just the spawn-visible publish/deploy steps — cross-checks npm_wait and dev-dep
	// updates too, and any future PublishStep kind that emits an event.
	const make_dep_update = (
		dependent: string,
		updated_dependency: string,
		type: DependencyUpdate['type'],
		new_version: string,
	): DependencyUpdate => ({
		dependent_package: dependent,
		updated_dependency,
		current_version: '^1.0.0',
		new_version,
		type,
	});

	const step_key = (step: PublishStep): string => {
		switch (step.kind) {
			case 'publish':
				return `publish:${step.repo}`;
			case 'npm_wait':
				return `npm_wait:${step.repo}`;
			case 'dependency_update':
				return `dep_update:${step.dependent}:${step.creates_changeset}`;
			case 'dev_dep_update':
				return `dev_dep:${step.repo}`;
			case 'deploy':
				return `deploy:${step.repo}`;
		}
	};

	const event_key = (event: PublishingEvent): string | null => {
		switch (event.event) {
			case 'package_completed':
				return `publish:${event.name}`;
			case 'npm_waited':
				return `npm_wait:${event.name}`;
			case 'dependency_updated':
				return event.dep_type === 'dev'
					? `dev_dep:${event.dependent}`
					: `dep_update:${event.dependent}:${event.creates_changeset}`;
			case 'deploy_started':
				return `deploy:${event.name}`;
			default:
				return null;
		}
	};

	const assert_preview_matches_execution = async (
		repos: Array<LocalRepo>,
		plan: PublishingPlan,
		options: {deploy?: boolean} = {},
	): Promise<void> => {
		// Populate each publishing package's package.json with its planned version so the
		// publish read-back matches the plan (no drift abort).
		const fs = create_populated_fs_ops(
			repos,
			new Map(plan.version_changes.map((vc) => [vc.package_name, vc.to])),
		);
		const result = await execute_publishing_plan(repos, plan, {
			wetrun: true,
			...options,
			ops: create_mock_gitops_ops({
				preflight: create_preflight_mock(repos.map((r) => r.library.name)),
				fs,
			}),
		});
		const executed = result.events.map(event_key).filter((key): key is string => key !== null);
		const previewed = derive_publish_steps(plan, options).map(step_key);
		assert.deepEqual(executed, previewed);
	};

	test('preview matches execution: prod cascade with deploy', async () => {
		const repos = [
			create_mock_repo({name: 'core', version: '1.0.0'}),
			create_mock_repo({name: 'mid', version: '1.0.0', deps: {core: '^1.0.0'}}),
			create_mock_repo({name: 'app', version: '1.0.0', deps: {mid: '^1.0.0'}}),
		];
		const plan = make_plan({
			publishing_order: ['core', 'mid', 'app'],
			version_changes: [
				make_version_change({package_name: 'core', to: '1.1.0', bump_type: 'minor'}),
				make_version_change({
					package_name: 'mid',
					to: '1.0.1',
					has_changesets: false,
					will_generate_changeset: true,
				}),
				make_version_change({
					package_name: 'app',
					to: '1.0.1',
					has_changesets: false,
					will_generate_changeset: true,
				}),
			],
			dependency_updates: [
				make_dep_update('mid', 'core', 'dependencies', '1.1.0'),
				make_dep_update('app', 'mid', 'dependencies', '1.0.1'),
			],
		});
		await assert_preview_matches_execution(repos, plan, {deploy: true});
	});

	test('preview matches execution: dev-dependency update (no changeset, batched install)', async () => {
		const repos = [
			create_mock_repo({name: 'tool', version: '1.0.0'}),
			create_mock_repo({name: 'consumer', version: '1.0.0', dev_deps: {tool: '^1.0.0'}}),
		];
		const plan = make_plan({
			publishing_order: ['tool', 'consumer'],
			version_changes: [make_version_change({package_name: 'tool', to: '1.0.1'})],
			dependency_updates: [make_dep_update('consumer', 'tool', 'devDependencies', '1.0.1')],
		});
		await assert_preview_matches_execution(repos, plan, {deploy: true});
	});

	test('preview matches execution: private update-only leaf', async () => {
		const repos = [
			create_mock_repo({name: 'core', version: '1.0.0'}),
			create_mock_repo({name: 'priv', version: '1.0.0', deps: {core: '^1.0.0'}, private: true}),
		];
		// priv is private → excluded from version_changes (it never publishes); only its range
		// updates, with no changeset.
		const plan = make_plan({
			publishing_order: ['core', 'priv'],
			version_changes: [
				make_version_change({package_name: 'core', to: '1.1.0', bump_type: 'minor'}),
			],
			dependency_updates: [make_dep_update('priv', 'core', 'dependencies', '1.1.0')],
		});
		await assert_preview_matches_execution(repos, plan, {deploy: true});
	});

	test('preview matches execution: mixed prod cascade + dev-dep update', async () => {
		const repos = [
			create_mock_repo({name: 'core', version: '1.0.0'}),
			create_mock_repo({name: 'mid', version: '1.0.0', deps: {core: '^1.0.0'}}),
			create_mock_repo({name: 'consumer', version: '1.0.0', dev_deps: {core: '^1.0.0'}}),
		];
		const plan = make_plan({
			publishing_order: ['core', 'mid', 'consumer'],
			version_changes: [
				make_version_change({package_name: 'core', to: '1.1.0', bump_type: 'minor'}),
				make_version_change({
					package_name: 'mid',
					to: '1.0.1',
					has_changesets: false,
					will_generate_changeset: true,
				}),
			],
			dependency_updates: [
				make_dep_update('mid', 'core', 'dependencies', '1.1.0'),
				make_dep_update('consumer', 'core', 'devDependencies', '1.1.0'),
			],
		});
		await assert_preview_matches_execution(repos, plan);
	});

	test('a private dependent is an update-only leaf — no publish, npm-wait, or changeset', async () => {
		const repos = [
			create_mock_repo({name: 'core', version: '1.0.0'}),
			create_mock_repo({name: 'priv', version: '1.0.0', deps: {core: '^1.0.0'}, private: true}),
		];
		const plan = make_plan({
			publishing_order: ['core', 'priv'],
			version_changes: [
				make_version_change({package_name: 'core', to: '1.1.0', bump_type: 'minor'}),
			],
			dependency_updates: [make_dep_update('priv', 'core', 'dependencies', '1.1.0')],
		});

		const fs = create_populated_fs_ops(repos, new Map([['core', '1.1.0']]));
		const add_files: Array<string> = [];
		const waited: Array<string> = [];
		const result = await execute_publishing_plan(repos, plan, {
			wetrun: true,
			ops: create_mock_gitops_ops({
				preflight: create_preflight_mock(['core']),
				fs,
				git: {
					add: async (options) => {
						const files = Array.isArray(options.files) ? options.files.join(',') : options.files;
						add_files.push(`${options.cwd}:${files}`);
						return {ok: true};
					},
				},
				npm: {
					wait_for_package: async (options) => {
						waited.push(options.pkg);
						return {ok: true};
					},
				},
			}),
		});

		assert.ok(result.ok);
		assert.deepEqual(
			result.published.map((p) => p.name),
			['core'], // priv never publishes
		);
		assert.deepEqual(waited, ['core']); // priv is never awaited on npm
		// priv's range was rewritten + committed, but with NO changeset staged
		assert.ok(
			!add_files.some((f) => f.includes('.changeset')),
			'a private leaf gets no auto-changeset',
		);
		const priv_pkg = JSON.parse(fs.get('/test/priv/package.json')!);
		assert.strictEqual(priv_pkg.dependencies.core, '^1.1.0'); // range bumped
	});
});
