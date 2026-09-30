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
 * Commits `files` alone with a message, leaving anything else staged out of
 * the commit, and throws if anything goes wrong. `files` must be non-empty:
 * an empty list would commit the whole index.
 */
export const git_commit = async (
	message: string,
	files: Array<string>,
	options?: SpawnOptions
): Promise<void> => {
	if (files.length === 0) {
		throw Error('git_commit needs at least one file: an empty list would commit the whole index');
	}
	const { result, stderr } = await spawn_out(
		'git',
		['commit', '-m', message, '--', ...files],
		options
	);
	if (!result.ok) {
		throw Error(
			`git_commit failed with ${spawn_result_to_message(result)}${stderr ? ': ' + stderr.trim() : ''}`
		);
	}
};

/**
 * Wrapper for gro's `git_current_commit_hash` that reads `HEAD` and throws if null.
 */
export const git_current_commit_hash_required = async (options?: SpawnOptions): Promise<string> => {
	const hash = await gro_git_current_commit_hash(undefined, options);
	if (!hash) {
		throw new Error('Failed to get the current commit hash');
	}
	return hash;
};
