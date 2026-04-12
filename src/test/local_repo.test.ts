import {assert, test} from 'vitest';
import {assert_rejects} from '@fuzdev/fuz_util/testing.js';

import {local_repo_load, local_repos_load, type LocalRepoPath} from '$lib/local_repo.js';
import {create_mock_git_ops, create_mock_npm_ops} from './test_helpers.js';

const create_local_repo_path = (name: string = 'test-repo'): LocalRepoPath => ({
	type: 'local_repo_path',
	repo_name: name,
	repo_dir: `/test/${name}`,
	repo_url: `https://github.com/test/${name}`,
	repo_git_ssh_url: `git@github.com:test/${name}.git`,
	repo_config: {
		repo_url: `https://github.com/test/${name}`,
		repo_dir: null,
		branch: 'main',
	},
});

// -- local_repo_load: operation-level failures --

test('current_commit_hash failure propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_commit_hash: async () => ({ok: false, message: 'not a git repository'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to get commit hash.*not a git repository/,
	);
});

test('current_branch_name failure propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ok: false, message: 'detached HEAD state'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to get current branch.*detached HEAD/,
	);
});

test('has_remote failure propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: false, message: 'git error'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to check for remote.*git error/,
	);
});

test('check_clean_workspace failure during branch switch propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ok: true, value: 'other-branch'}),
					check_clean_workspace: async () => ({ok: false, message: 'git status failed'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to check workspace.*git status failed/,
	);
});

test('check_clean_workspace failure after pull propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					// on correct branch so no branch-switch check — this is the post-pull check
					check_clean_workspace: async () => ({ok: false, message: 'git status timed out'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to check workspace.*git status timed out/,
	);
});

test('post-pull current_commit_hash failure propagates', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					current_commit_hash: async () => {
						call++;
						if (call <= 1) return {ok: true, value: 'aaa'};
						return {ok: false, message: 'corrupt ref'};
					},
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to get commit hash.*corrupt ref/,
	);
});

test('has_file_changed failure propagates', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					current_commit_hash: async () => {
						call++;
						return {ok: true, value: call <= 1 ? 'aaa' : 'bbb'};
					},
					has_file_changed: async () => ({ok: false, message: 'diff failed'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to check if package\.json changed.*diff failed/,
	);
});

// -- local_repo_load: behavioral errors --

test('pull failure includes message', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					pull: async () => ({
						ok: false,
						message: 'cannot pull with rebase: You have unstaged changes.',
					}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to pull in \/test\/test-repo.*unstaged changes/,
	);
});

test('checkout failure includes message', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ok: true, value: 'other-branch'}),
					checkout: async () => ({
						ok: false,
						message: "pathspec 'main' did not match any file(s)",
					}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to checkout branch "main" in \/test\/test-repo.*pathspec/,
	);
});

test('dirty workspace blocks branch switch', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ok: true, value: 'other-branch'}),
					check_clean_workspace: async () => ({ok: true, value: false}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/not on branch "main" and the workspace is unclean/,
	);
});

test('dirty workspace after pull', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					// on correct branch so no branch-switch check — this is the post-pull check
					check_clean_workspace: async () => ({ok: true, value: false}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/unclean after pulling branch "main"/,
	);
});

test('install failure includes stderr', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					current_commit_hash: async () => {
						call++;
						return {ok: true, value: call <= 1 ? 'aaa' : 'bbb'};
					},
					has_file_changed: async () => ({ok: true, value: true}),
				}),
				npm_ops: create_mock_npm_ops({
					install: async () => ({
						ok: false,
						message: 'Install failed',
						stderr: 'npm ERR! ERESOLVE could not resolve',
					}),
				}),
			}),
		/Failed to install dependencies[\s\S]*ERESOLVE/,
	);
});

// -- local_repo_load: skip behaviors --

test('local-only repos skip pull', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: false}),
					// pull would fail if called — reaching library.ts error proves it was skipped
					pull: async () => ({ok: false, message: 'pull should not be called'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/missing src\/routes\/library\.ts/,
	);
});

test('skips install when no new commits', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					// default mock returns same hash for both calls → no new commits
				}),
				npm_ops: create_mock_npm_ops({
					// install would fail if called — reaching library.ts error proves it was skipped
					install: async () => ({ok: false, message: 'install should not be called'}),
				}),
			}),
		/missing src\/routes\/library\.ts/,
	);
});

test('skips install when package.json unchanged', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					current_commit_hash: async () => {
						call++;
						return {ok: true, value: call <= 1 ? 'aaa' : 'bbb'};
					},
					has_file_changed: async () => ({ok: true, value: false}),
				}),
				npm_ops: create_mock_npm_ops({
					// install would fail if called — reaching library.ts error proves it was skipped
					install: async () => ({ok: false, message: 'install should not be called'}),
				}),
			}),
		/missing src\/routes\/library\.ts/,
	);
});

// -- local_repos_load --

test('local_repos_load aggregates errors from multiple repos', async () => {
	const err = await assert_rejects(
		() =>
			local_repos_load({
				local_repo_paths: [create_local_repo_path('repo-a'), create_local_repo_path('repo-b')],
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					pull: async () => ({ok: false, message: 'unstaged changes'}),
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to load 2 repos/,
	);
	assert.include(err.message, 'repo-a');
	assert.include(err.message, 'repo-b');
});

test('local_repos_load includes per-repo error details', async () => {
	const err = await assert_rejects(
		() =>
			local_repos_load({
				local_repo_paths: [create_local_repo_path('repo-ok'), create_local_repo_path('repo-bad')],
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					pull: async (options) => {
						if (options?.cwd?.includes('repo-bad')) {
							return {ok: false, message: 'unstaged changes'};
						}
						return {ok: true};
					},
				}),
				npm_ops: create_mock_npm_ops(),
			}),
		/Failed to load 2 repos/,
	);
	// repo-bad fails at pull with specific message
	assert.include(err.message, 'repo-bad');
	assert.include(err.message, 'unstaged changes');
	// repo-ok fails later at library.ts import
	assert.include(err.message, 'repo-ok');
	assert.include(err.message, 'library.ts');
});

test('local_repos_load sequential mode throws on first failure', async () => {
	await assert_rejects(
		() =>
			local_repos_load({
				local_repo_paths: [create_local_repo_path('repo-a'), create_local_repo_path('repo-b')],
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ok: true, value: true}),
					pull: async () => ({ok: false, message: 'network error'}),
				}),
				npm_ops: create_mock_npm_ops(),
				parallel: false,
			}),
		/Failed to pull.*network error/,
	);
});
