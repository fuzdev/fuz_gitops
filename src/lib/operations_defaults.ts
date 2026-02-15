/**
 * Production implementations of operations interfaces.
 *
 * Provides real git, npm, fs, and build operations for production use.
 * For interface definitions and dependency injection pattern, see `operations.ts`.
 *
 * @module
 */

import {spawn, spawn_out} from '@fuzdev/fuz_util/process.js';
import {readFile, writeFile, mkdir} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import {git_checkout, type GitBranch, type GitOrigin} from '@fuzdev/fuz_util/git.js';
import {EMPTY_OBJECT} from '@fuzdev/fuz_util/object.js';

import {has_changesets, read_changesets, predict_next_version} from './changeset_reader.js';
import {wait_for_package, check_package_available} from './npm_registry.js';
import {run_preflight_checks} from './preflight_checks.js';
import {
	git_add,
	git_commit,
	git_add_and_commit,
	git_tag,
	git_push_tag,
	git_has_changes,
	git_get_changed_files,
	git_has_file_changed,
	git_stash,
	git_stash_pop,
	git_switch_branch,
	git_current_branch_name_required,
	git_current_commit_hash_required,
	git_check_clean_workspace_as_boolean,
	git_has_remote,
} from './git_operations.js';
import type {
	ChangesetOperations,
	GitOperations,
	ProcessOperations,
	NpmOperations,
	PreflightOperations,
	FsOperations,
	BuildOperations,
	GitopsOperations,
} from './operations.js';

/** Wrap an async function that returns a value */
const wrap_with_value = async <T>(
	fn: () => Promise<T>,
): Promise<{ok: true; value: T} | {ok: false; message: string}> => {
	try {
		const value = await fn();
		return {ok: true, value};
	} catch (error) {
		return {ok: false, message: String(error)};
	}
};

/** Wrap an async function, ignoring its return value */
const wrap_void = async (
	fn: () => Promise<unknown>,
): Promise<{ok: true} | {ok: false; message: string}> => {
	try {
		await fn();
		return {ok: true};
	} catch (error) {
		return {ok: false, message: String(error)};
	}
};

export const default_changeset_operations: ChangesetOperations = {
	has_changesets: async (options) => {
		const {repo} = options;
		return wrap_with_value(() => has_changesets(repo));
	},

	read_changesets: async (options) => {
		const {repo, log} = options;
		return wrap_with_value(() => read_changesets(repo, log));
	},

	predict_next_version: async (options) => {
		const {repo, log} = options;
		try {
			const result = await predict_next_version(repo, log);
			if (result === null) {
				return null;
			}
			return {ok: true, ...result};
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},
};

export const default_git_operations: GitOperations = {
	// Core git info
	current_branch_name: async (options) => {
		const {cwd} = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_current_branch_name_required(cwd ? {cwd} : undefined));
	},

	current_commit_hash: async (options) => {
		const {branch, cwd} = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_current_commit_hash_required(branch, cwd ? {cwd} : undefined));
	},

	check_clean_workspace: async (options) => {
		const {cwd} = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_check_clean_workspace_as_boolean(cwd ? {cwd} : undefined));
	},

	// Branch operations
	checkout: async (options) => {
		const {branch, cwd} = options;
		return wrap_void(() => git_checkout(branch, cwd ? {cwd} : undefined));
	},

	pull: async (options) => {
		const {origin, branch, cwd} = options ?? EMPTY_OBJECT;
		return wrap_void(() =>
			spawn('git', ['pull', origin || 'origin', branch || ''], cwd ? {cwd} : undefined),
		);
	},

	switch_branch: async (options) => {
		const {branch, pull, cwd} = options;
		return wrap_void(() => git_switch_branch(branch as GitBranch, pull, cwd ? {cwd} : undefined));
	},

	has_remote: async (options) => {
		const {remote, cwd} = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_has_remote(remote, cwd ? {cwd} : undefined));
	},

	// Staging and committing
	add: async (options) => {
		const {files, cwd} = options;
		return wrap_void(() => git_add(files, cwd ? {cwd} : undefined));
	},

	commit: async (options) => {
		const {message, cwd} = options;
		return wrap_void(() => git_commit(message, cwd ? {cwd} : undefined));
	},

	add_and_commit: async (options) => {
		const {files, message, cwd} = options;
		return wrap_void(() => git_add_and_commit(files, message, cwd ? {cwd} : undefined));
	},

	has_changes: async (options) => {
		const {cwd} = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_has_changes(cwd ? {cwd} : undefined));
	},

	get_changed_files: async (options) => {
		const {cwd} = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_get_changed_files(cwd ? {cwd} : undefined));
	},

	// Tagging
	tag: async (options) => {
		const {tag_name, message, cwd} = options;
		return wrap_void(() => git_tag(tag_name, message, cwd ? {cwd} : undefined));
	},

	push_tag: async (options) => {
		const {tag_name, origin, cwd} = options;
		return wrap_void(() => git_push_tag(tag_name, origin as GitOrigin, cwd ? {cwd} : undefined));
	},

	// Stashing
	stash: async (options) => {
		const {message, cwd} = options ?? EMPTY_OBJECT;
		return wrap_void(() => git_stash(message, cwd ? {cwd} : undefined));
	},

	stash_pop: async (options) => {
		const {cwd} = options ?? EMPTY_OBJECT;
		return wrap_void(() => git_stash_pop(cwd ? {cwd} : undefined));
	},

	// File change detection
	has_file_changed: async (options) => {
		const {from_commit, to_commit, file_path, cwd} = options;
		return wrap_with_value(() =>
			git_has_file_changed(from_commit, to_commit, file_path, cwd ? {cwd} : undefined),
		);
	},
};

export const default_process_operations: ProcessOperations = {
	spawn: async (options) => {
		const {cmd, args, spawn_options} = options;
		try {
			const spawned = await spawn_out(cmd, args, spawn_options);
			if (spawned.result.ok) {
				return {
					ok: true,
					stdout: spawned.stdout || undefined,
					stderr: spawned.stderr || undefined,
				};
			} else {
				return {
					ok: false,
					message: 'Command failed',
					stderr: spawned.stderr || undefined,
				};
			}
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},
};

export const default_npm_operations: NpmOperations = {
	wait_for_package: async (options) => {
		const {pkg, version, wait_options, log} = options;
		try {
			await wait_for_package(pkg, version, {...wait_options, log});
			return {ok: true};
		} catch (error) {
			return {ok: false, message: String(error), timeout: true};
		}
	},

	check_package_available: async (options) => {
		const {pkg, version, log} = options;
		return wrap_with_value(() => check_package_available(pkg, version, {log}));
	},

	check_auth: async () => {
		try {
			const result = await spawn_out('npm', ['whoami']);
			if (result.stdout) {
				const username = result.stdout.trim();
				if (username) {
					return {ok: true, username};
				}
			}
			return {ok: false, message: 'Not logged in to npm'};
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},

	check_registry: async () => {
		try {
			const result = await spawn_out('npm', ['ping']);
			if (result.stdout) {
				return {ok: true};
			}
			return {ok: false, message: 'Failed to ping npm registry'};
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},

	install: async (options) => {
		const {cwd} = options ?? EMPTY_OBJECT;
		try {
			const spawned = await spawn_out('npm', ['install'], cwd ? {cwd} : undefined);
			if (spawned.result.ok) {
				return {ok: true};
			} else {
				return {ok: false, message: 'Install failed', stderr: spawned.stderr || undefined};
			}
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},

	cache_clean: async () => {
		try {
			const spawned = await spawn_out('npm', ['cache', 'clean', '--force']);
			if (spawned.result.ok) {
				return {ok: true};
			} else {
				return {ok: false, message: 'Cache clean failed'};
			}
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},
};

export const default_preflight_operations: PreflightOperations = {
	run_preflight_checks: async (options) => {
		return run_preflight_checks(options);
	},
};

export const default_fs_operations: FsOperations = {
	readFile: async (options) => {
		const {path, encoding} = options;
		return wrap_with_value(() => readFile(path, encoding));
	},

	writeFile: async (options) => {
		const {path, content} = options;
		return wrap_void(() => writeFile(path, content));
	},

	mkdir: async (options) => {
		const {path, recursive} = options;
		return wrap_void(() => mkdir(path, {recursive}));
	},

	exists: (options) => {
		return existsSync(options.path);
	},
};

export const default_build_operations: BuildOperations = {
	build_package: async (options) => {
		const {repo, log} = options;
		try {
			log?.info(`  Building ${repo.library.name}...`);
			const spawned = await spawn_out('gro', ['build'], {cwd: repo.repo_dir});
			if (spawned.result.ok) {
				return {ok: true};
			} else {
				return {
					ok: false,
					message: 'Build failed',
					output: spawned.stderr || spawned.stdout || 'Build failed',
				};
			}
		} catch (error) {
			return {ok: false, message: String(error)};
		}
	},
};

/**
 * Combined default operations for all gitops functionality.
 */
export const default_gitops_operations: GitopsOperations = {
	changeset: default_changeset_operations,
	git: default_git_operations,
	process: default_process_operations,
	npm: default_npm_operations,
	preflight: default_preflight_operations,
	fs: default_fs_operations,
	build: default_build_operations,
};
