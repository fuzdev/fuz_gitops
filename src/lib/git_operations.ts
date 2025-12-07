import {spawn, spawn_out} from '@fuzdev/fuz_util/process.js';
import type {SpawnOptions} from 'node:child_process';
import {
	git_check_clean_workspace as gro_git_check_clean_workspace,
	git_checkout as gro_git_checkout,
	git_pull as gro_git_pull,
	git_current_branch_name as gro_git_current_branch_name,
	git_current_commit_hash as gro_git_current_commit_hash,
	type GitBranch,
	type GitOrigin,
} from '@fuzdev/fuz_util/git.js';

/**
 * Adds files to git staging area and throws if anything goes wrong.
 */
export const git_add = async (
	files: string | Array<string>,
	options?: SpawnOptions,
): Promise<void> => {
	const file_list = Array.isArray(files) ? files : [files];
	const result = await spawn('git', ['add', ...file_list], options);
	if (!result.ok) {
		throw Error(`git_add failed with code ${result.code}`);
	}
};

/**
 * Commits staged changes with a message and throws if anything goes wrong.
 */
export const git_commit = async (message: string, options?: SpawnOptions): Promise<void> => {
	const result = await spawn('git', ['commit', '-m', message], options);
	if (!result.ok) {
		throw Error(`git_commit failed with code ${result.code}`);
	}
};

/**
 * Adds files and commits in one operation and throws if anything goes wrong.
 */
export const git_add_and_commit = async (
	files: string | Array<string>,
	message: string,
	options?: SpawnOptions,
): Promise<void> => {
	await git_add(files, options);
	await git_commit(message, options);
};

/**
 * Creates a git tag and throws if anything goes wrong.
 */
export const git_tag = async (
	tag_name: string,
	message?: string,
	options?: SpawnOptions,
): Promise<void> => {
	const args = message ? ['tag', '-a', tag_name, '-m', message] : ['tag', tag_name];

	const result = await spawn('git', args, options);
	if (!result.ok) {
		throw Error(`git_tag failed for tag '${tag_name}' with code ${result.code}`);
	}
};

/**
 * Pushes a tag to origin and throws if anything goes wrong.
 */
export const git_push_tag = async (
	tag_name: string,
	origin: GitOrigin = 'origin' as GitOrigin,
	options?: SpawnOptions,
): Promise<void> => {
	const result = await spawn('git', ['push', origin, tag_name], options);
	if (!result.ok) {
		throw Error(`git_push_tag failed for tag '${tag_name}' with code ${result.code}`);
	}
};

export const git_has_changes = async (options?: SpawnOptions): Promise<boolean> => {
	const {stdout} = await spawn_out('git', ['status', '--porcelain'], options);
	return stdout ? stdout.trim().length > 0 : false;
};

/**
 * Returns list of changed files compared to HEAD.
 */
export const git_get_changed_files = async (options?: SpawnOptions): Promise<Array<string>> => {
	const {stdout} = await spawn_out('git', ['diff', '--name-only', 'HEAD'], options);
	if (!stdout) return [];

	return stdout
		.split('\n')
		.map((f) => f.trim())
		.filter(Boolean);
};

export const git_has_file_changed = async (
	from_commit: string,
	to_commit: string,
	file_path: string,
	options?: SpawnOptions,
): Promise<boolean> => {
	const {stdout} = await spawn_out(
		'git',
		['diff', '--name-only', from_commit, to_commit, '--', file_path],
		options,
	);
	return stdout ? stdout.trim().length > 0 : false;
};

/**
 * Stashes current changes and throws if anything goes wrong.
 */
export const git_stash = async (message?: string, options?: SpawnOptions): Promise<void> => {
	const args = message ? ['stash', 'push', '-m', message] : ['stash', 'push'];

	const result = await spawn('git', args, options);
	if (!result.ok) {
		throw Error(`git_stash failed with code ${result.code}`);
	}
};

/**
 * Applies stashed changes and throws if anything goes wrong.
 */
export const git_stash_pop = async (options?: SpawnOptions): Promise<void> => {
	const result = await spawn('git', ['stash', 'pop'], options);
	if (!result.ok) {
		throw Error(`git_stash_pop failed with code ${result.code}`);
	}
};

/**
 * Switches to a branch with safety checks and throws if workspace is not clean.
 */
export const git_switch_branch = async (
	branch: GitBranch,
	pull: boolean = true,
	options?: SpawnOptions,
): Promise<void> => {
	// Check if workspace is clean first
	const error = await gro_git_check_clean_workspace(options);
	if (error) {
		throw Error(`Cannot switch branch: ${error}`);
	}

	// Checkout the branch
	await gro_git_checkout(branch, options);

	// Pull latest changes if requested
	if (pull) {
		await gro_git_pull(undefined, undefined, options);
	}

	// Verify workspace is still clean
	const error_after = await gro_git_check_clean_workspace(options);
	if (error_after) {
		throw Error(`Workspace unclean after switching to ${branch}: ${error_after}`);
	}
};

/**
 * Wrapper for gro's git_current_branch_name that throws if null.
 */
export const git_current_branch_name_required = async (options?: SpawnOptions): Promise<string> => {
	const branch = await gro_git_current_branch_name(options);
	if (!branch) {
		throw new Error('Failed to get current branch name');
	}
	return branch;
};

/**
 * Wrapper for gro's git_current_commit_hash that throws if null.
 */
export const git_current_commit_hash_required = async (
	branch?: string,
	options?: SpawnOptions,
): Promise<string> => {
	const hash = await gro_git_current_commit_hash(branch, options);
	if (!hash) {
		throw new Error(`Failed to get commit hash for branch: ${branch || 'current'}`);
	}
	return hash;
};

/**
 * Wrapper for gro's git_check_clean_workspace that returns a boolean.
 */
export const git_check_clean_workspace_as_boolean = async (
	options?: SpawnOptions,
): Promise<boolean> => {
	const error = await gro_git_check_clean_workspace(options);
	return error === null;
};

export const git_has_remote = async (
	remote: string = 'origin',
	options?: SpawnOptions,
): Promise<boolean> => {
	const {stdout} = await spawn_out('git', ['remote'], options);
	if (!stdout) return false;
	const remotes = stdout
		.split('\n')
		.map((r) => r.trim())
		.filter(Boolean);
	return remotes.includes(remote);
};
