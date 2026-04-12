import {assert, describe, test} from 'vitest';
import {join} from 'node:path';

import {
	update_package_json,
	update_all_repos,
	find_updates_needed,
} from '$lib/dependency_updater.js';
import {create_mock_repo, create_mock_git_ops, create_mock_fs_ops} from './test_helpers.js';
import type {GitOperations} from '$lib/operations.js';

/**
 * Creates mock git operations that track calls
 */
const create_trackable_git_ops = (): GitOperations & {
	added_files: Array<string>;
	commits: Array<string>;
} => {
	const added_files: Array<string> = [];
	const commits: Array<string> = [];

	const git_ops = create_mock_git_ops({
		add: async (options) => {
			if (Array.isArray(options.files)) {
				added_files.push(...options.files);
			} else {
				added_files.push(options.files);
			}
			return {ok: true};
		},
		commit: async (options) => {
			commits.push(options.message);
			return {ok: true};
		},
		add_and_commit: async (options) => {
			if (Array.isArray(options.files)) {
				added_files.push(...options.files);
			} else {
				added_files.push(options.files);
			}
			commits.push(options.message);
			return {ok: true};
		},
	});

	return {
		...git_ops,
		added_files,
		commits,
	};
};

describe('dependency_updater', () => {
	describe('update_package_json', () => {
		test('updates production dependencies with caret prefix', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '^1.0.0',
					'dep-b': '^2.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {
							'dep-a': '^1.0.0',
							'dep-b': '^2.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dep-a', '1.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			assert.ok(updated !== undefined);

			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.dependencies['dep-a'], '^1.1.0');
			assert.strictEqual(parsed.dependencies['dep-b'], '^2.0.0'); // unchanged
		});

		test('updates devDependencies', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				dev_deps: {
					'dev-a': '^1.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						devDependencies: {
							'dev-a': '^1.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dev-a', '2.0.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.devDependencies['dev-a'], '^2.0.0');
		});

		test('updates peerDependencies', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				peer_deps: {
					'peer-a': '^3.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						peerDependencies: {
							'peer-a': '^3.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['peer-a', '3.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.peerDependencies['peer-a'], '^3.1.0');
		});

		test('preserves tilde prefix when using tilde strategy', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '~1.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {
							'dep-a': '~1.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dep-a', '1.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {strategy: 'tilde', git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.dependencies['dep-a'], '~1.1.0');
		});

		test('uses exact versions with exact strategy', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '1.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {
							'dep-a': '1.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dep-a', '1.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {strategy: 'exact', git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.dependencies['dep-a'], '1.1.0'); // no prefix
		});

		test('preserves >= prefix in peerDependencies', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				peer_deps: {
					'@fuzdev/fuz_util': '>=0.38.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						peerDependencies: {
							'@fuzdev/fuz_util': '>=0.38.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['@fuzdev/fuz_util', '0.39.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.peerDependencies['@fuzdev/fuz_util'], '>=0.39.0');
		});

		test('uses gte strategy for >= prefix on new deps', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '1.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {
							'dep-a': '1.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dep-a', '1.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {strategy: 'gte', git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.dependencies['dep-a'], '>=1.1.0');
		});

		test('updates multiple dependencies at once', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '^1.0.0',
					'dep-b': '^2.0.0',
				},
				dev_deps: {
					'dev-a': '^3.0.0',
				},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {
							'dep-a': '^1.0.0',
							'dep-b': '^2.0.0',
						},
						devDependencies: {
							'dev-a': '^3.0.0',
						},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([
				['dep-a', '1.2.0'],
				['dep-b', '2.5.0'],
				['dev-a', '3.1.0'],
			]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			const parsed = JSON.parse(updated!);
			assert.strictEqual(parsed.dependencies['dep-a'], '^1.2.0');
			assert.strictEqual(parsed.dependencies['dep-b'], '^2.5.0');
			assert.strictEqual(parsed.devDependencies['dev-a'], '^3.1.0');
		});

		test('preserves JSON formatting with tabs', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {'dep-a': '^1.0.0'},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {'dep-a': '^1.0.0'},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dep-a', '1.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			const updated = fs.get(package_json_path);
			// Check it has tabs (JSON.stringify uses tabs)
			assert.ok(updated!.includes('\t'));
			assert.match(updated!, /\n$/); // ends with newline
		});

		test('creates git commit with correct message', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {'dep-a': '^1.0.0'},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {'dep-a': '^1.0.0'},
					},
					null,
					'\t',
				),
			);

			const updates = new Map([['dep-a', '1.1.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			assert.ok(git_ops.added_files.includes('package.json'));
			assert.strictEqual(git_ops.commits.length, 1);
			assert.ok(git_ops.commits[0]!.includes('update dependencies after publishing'));
		});

		test('does nothing when updates map is empty', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({name: 'test-pkg'});

			const updates: Map<string, string> = new Map();
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			assert.strictEqual(git_ops.added_files.length, 0);
			assert.strictEqual(git_ops.commits.length, 0);
		});

		test('does nothing when no matching dependencies found', async () => {
			const fs = create_mock_fs_ops();
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {'dep-a': '^1.0.0'},
			});

			const package_json_path = join(repo.repo_dir, 'package.json');
			fs.set(
				package_json_path,
				JSON.stringify(
					{
						name: 'test-pkg',
						version: '1.0.0',
						dependencies: {'dep-a': '^1.0.0'},
					},
					null,
					'\t',
				),
			);

			// Update for a different dependency
			const updates = new Map([['dep-b', '2.0.0']]);
			const git_ops = create_trackable_git_ops();

			await update_package_json(repo, updates, {git_ops, fs_ops: fs});

			assert.strictEqual(git_ops.commits.length, 0);
		});
	});

	describe('find_updates_needed', () => {
		test('identifies dependencies that need updating', () => {
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '^1.0.0',
					'dep-b': '^2.0.0',
				},
			});

			const published = new Map([
				['dep-a', '1.1.0'],
				['dep-b', '2.0.0'], // same version
			]);

			const updates = find_updates_needed(repo, published);

			assert.strictEqual(updates.size, 1);
			assert.deepEqual(updates.get('dep-a'), {
				current: '^1.0.0',
				new: '1.1.0',
				type: 'dependencies',
			});
		});

		test('identifies devDependencies needing updates', () => {
			const repo = create_mock_repo({
				name: 'test-pkg',
				dev_deps: {
					'dev-a': '^3.0.0',
				},
			});

			const published = new Map([['dev-a', '3.5.0']]);

			const updates = find_updates_needed(repo, published);

			assert.strictEqual(updates.size, 1);
			assert.deepEqual(updates.get('dev-a'), {
				current: '^3.0.0',
				new: '3.5.0',
				type: 'devDependencies',
			});
		});

		test('identifies peerDependencies needing updates', () => {
			const repo = create_mock_repo({
				name: 'test-pkg',
				peer_deps: {
					'peer-a': '^4.0.0',
				},
			});

			const published = new Map([['peer-a', '4.1.0']]);

			const updates = find_updates_needed(repo, published);

			assert.strictEqual(updates.size, 1);
			assert.deepEqual(updates.get('peer-a'), {
				current: '^4.0.0',
				new: '4.1.0',
				type: 'peerDependencies',
			});
		});

		test('returns empty map when no updates needed', () => {
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {
					'dep-a': '^1.0.0',
				},
			});

			const published = new Map([['dep-a', '1.0.0']]);

			const updates = find_updates_needed(repo, published);

			assert.strictEqual(updates.size, 0);
		});

		test('handles multiple dependency types together', () => {
			const repo = create_mock_repo({
				name: 'test-pkg',
				deps: {'dep-a': '^1.0.0'},
				dev_deps: {'dev-a': '^2.0.0'},
				peer_deps: {'peer-a': '^3.0.0'},
			});

			const published = new Map([
				['dep-a', '1.1.0'],
				['dev-a', '2.2.0'],
				['peer-a', '3.3.0'],
			]);

			const updates = find_updates_needed(repo, published);

			assert.strictEqual(updates.size, 3);
			assert.strictEqual(updates.get('dep-a')?.type, 'dependencies');
			assert.strictEqual(updates.get('dev-a')?.type, 'devDependencies');
			assert.strictEqual(updates.get('peer-a')?.type, 'peerDependencies');
		});
	});

	describe('update_all_repos', () => {
		test('updates all repos with matching dependencies', async () => {
			const fs = create_mock_fs_ops();

			const repos = [
				create_mock_repo({name: 'pkg-a', deps: {lib: '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', deps: {lib: '^1.0.0'}}),
			];

			for (const repo of repos) {
				const path = join(repo.repo_dir, 'package.json');
				fs.set(
					path,
					JSON.stringify(
						{
							name: repo.library.name,
							version: '1.0.0',
							dependencies: {lib: '^1.0.0'},
						},
						null,
						'\t',
					),
				);
			}

			const published = new Map([['lib', '1.5.0']]);
			const git_ops = create_trackable_git_ops();

			const result = await update_all_repos(repos, published, {git_ops, fs_ops: fs});

			assert.strictEqual(result.updated, 2);
			assert.strictEqual(result.failed.length, 0);
		});

		test('skips repos without matching dependencies', async () => {
			const fs = create_mock_fs_ops();

			const repos = [
				create_mock_repo({name: 'pkg-a', deps: {lib: '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', deps: {other: '^2.0.0'}}),
			];

			for (const repo of repos) {
				const path = join(repo.repo_dir, 'package.json');
				fs.set(
					path,
					JSON.stringify(
						{
							name: repo.library.name,
							version: '1.0.0',
							dependencies: repo.library.package_json.dependencies,
						},
						null,
						'\t',
					),
				);
			}

			const published = new Map([['lib', '1.5.0']]);
			const git_ops = create_trackable_git_ops();

			const result = await update_all_repos(repos, published, {git_ops, fs_ops: fs});

			assert.strictEqual(result.updated, 1); // only pkg-a
		});

		test('reports failures for problematic repos', async () => {
			const fs = create_mock_fs_ops();

			const repos = [create_mock_repo({name: 'pkg-a', deps: {lib: '^1.0.0'}})];

			// Don't set up the file - will cause read error

			const published = new Map([['lib', '1.5.0']]);
			const git_ops = create_trackable_git_ops();

			const result = await update_all_repos(repos, published, {git_ops, fs_ops: fs});

			assert.strictEqual(result.updated, 0);
			assert.strictEqual(result.failed.length, 1);
			assert.strictEqual(result.failed[0]!.repo, 'pkg-a');
			assert.instanceOf(result.failed[0]!.error, Error);
		});
	});
});
