import { spawn_out, spawn_result_to_message } from '@fuzdev/fuz_util/process.ts';
import type { SpawnOptions } from 'node:child_process';
import { git_current_commit_hash as gro_git_current_commit_hash } from '@fuzdev/fuz_util/git.ts';

/**
 * Adds files to git staging area and throws if anything goes wrong.
 */
export const git_add = async (
	files: string | Array<string>,
	options?: SpawnOptions
): Promise<void> => {
	const file_list = Array.isArray(files) ? files : [files];
	const { result, stderr } = await spawn_out('git', ['add', ...file_list], options);
	if (!result.ok) {
		throw Error(
			`git_add failed with ${spawn_result_to_message(result)}${stderr ? ': ' + stderr.trim() : ''}`
		);
	}
};

/**
 * Commits staged changes with a message and throws if anything goes wrong.
 */
export const git_commit = async (message: string, options?: SpawnOptions): Promise<void> => {
	const { result, stderr } = await spawn_out('git', ['commit', '-m', message], options);
	if (!result.ok) {
		throw Error(
			`git_commit failed with ${spawn_result_to_message(result)}${stderr ? ': ' + stderr.trim() : ''}`
		);
	}
};

/**
 * Wrapper for gro's `git_current_commit_hash` that throws if null.
 */
export const git_current_commit_hash_required = async (
	branch?: string,
	options?: SpawnOptions
): Promise<string> => {
	const hash = await gro_git_current_commit_hash(branch, options);
	if (!hash) {
		throw new Error(`Failed to get commit hash for branch: ${branch || 'current'}`);
	}
	return hash;
};
