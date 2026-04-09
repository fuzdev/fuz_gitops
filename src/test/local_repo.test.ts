import {test} from 'vitest';

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

test('pull failure includes stderr in error message', async ({expect}) => {
	const git_ops = create_mock_git_ops({
		has_remote: async () => ({ok: true, value: true}),
		pull: async () => ({
			ok: false,
			message: 'cannot pull with rebase: You have unstaged changes.',
		}),
	});

	await expect(
		local_repo_load({
			local_repo_path: create_local_repo_path(),
			git_ops,
			npm_ops: create_mock_npm_ops(),
		}),
	).rejects.toThrow(/Failed to pull in \/test\/test-repo.*unstaged changes/);
});

test('checkout failure includes message', async ({expect}) => {
	const git_ops = create_mock_git_ops({
		current_branch_name: async () => ({ok: true, value: 'other-branch'}),
		checkout: async () => ({ok: false, message: "pathspec 'main' did not match any file(s)"}),
	});

	await expect(
		local_repo_load({
			local_repo_path: create_local_repo_path(),
			git_ops,
			npm_ops: create_mock_npm_ops(),
		}),
	).rejects.toThrow(/Failed to checkout branch "main" in \/test\/test-repo.*pathspec/);
});

test('dirty workspace blocks branch switch', async ({expect}) => {
	const git_ops = create_mock_git_ops({
		current_branch_name: async () => ({ok: true, value: 'other-branch'}),
		check_clean_workspace: async () => ({ok: true, value: false}),
	});

	await expect(
		local_repo_load({
			local_repo_path: create_local_repo_path(),
			git_ops,
			npm_ops: create_mock_npm_ops(),
		}),
	).rejects.toThrow(/not on branch "main" and the workspace is unclean/);
});

test('dirty workspace after pull', async ({expect}) => {
	const git_ops = create_mock_git_ops({
		has_remote: async () => ({ok: true, value: true}),
		// Already on correct branch so no branch-switch check — this is the post-pull check
		check_clean_workspace: async () => ({ok: true, value: false}),
	});

	await expect(
		local_repo_load({
			local_repo_path: create_local_repo_path(),
			git_ops,
			npm_ops: create_mock_npm_ops(),
		}),
	).rejects.toThrow(/unclean after pulling branch "main"/);
});

test('install failure includes stderr', async ({expect}) => {
	let hash_call = 0;
	const git_ops = create_mock_git_ops({
		has_remote: async () => ({ok: true, value: true}),
		current_commit_hash: async () => {
			hash_call++;
			// Different hashes to trigger "got new commits"
			return {ok: true, value: hash_call <= 1 ? 'aaa' : 'bbb'};
		},
		has_file_changed: async () => ({ok: true, value: true}),
	});

	const npm_ops = create_mock_npm_ops({
		install: async () => ({
			ok: false,
			message: 'Install failed',
			stderr: 'npm ERR! ERESOLVE could not resolve',
		}),
	});

	await expect(
		local_repo_load({
			local_repo_path: create_local_repo_path(),
			git_ops,
			npm_ops,
		}),
	).rejects.toThrow(/Failed to install dependencies[\s\S]*ERESOLVE/);
});

test('local_repos_load aggregates errors from multiple repos', async ({expect}) => {
	const paths = [create_local_repo_path('repo-a'), create_local_repo_path('repo-b')];

	const git_ops = create_mock_git_ops({
		has_remote: async () => ({ok: true, value: true}),
		pull: async () => ({
			ok: false,
			message: 'cannot pull with rebase: You have unstaged changes.',
		}),
	});

	await expect(
		local_repos_load({
			local_repo_paths: paths,
			git_ops,
			npm_ops: create_mock_npm_ops(),
		}),
	).rejects.toThrow(/Failed to load 2 repos.*repo-a.*repo-b/s);
});

test('local_repos_load reports only failed repos', async ({expect}) => {
	const paths = [create_local_repo_path('repo-ok'), create_local_repo_path('repo-bad')];

	// Make pull fail only for repo-bad
	const git_ops = create_mock_git_ops({
		has_remote: async () => ({ok: true, value: true}),
		pull: async (options) => {
			if (options?.cwd?.includes('repo-bad')) {
				return {ok: false, message: 'unstaged changes'};
			}
			return {ok: true};
		},
		// repo-ok will fail at existsSync (library.ts), but that throws differently
	});

	// Both will fail (repo-ok at library.ts import, repo-bad at pull), but with different errors
	const error = await local_repos_load({
		local_repo_paths: paths,
		git_ops,
		npm_ops: create_mock_npm_ops(),
	}).catch((e: Error) => e);

	expect(error).toBeInstanceOf(Error);
	expect(error!.message).toContain('repo-bad');
	expect(error!.message).toContain('unstaged changes');
});
