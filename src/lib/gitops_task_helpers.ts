/**
 * Shared initialization logic for all gitops tasks.
 *
 * `resolve_gitops_repos()` loads the config's registry keys, runs
 * `repos status <keys…> --json`, and resolves each key to its checkout.
 * `get_gitops_ready()` then loads each repo's library, optionally syncing its
 * working tree first (switch branch, pull, install).
 *
 * Used by: `gitops_sync.task.ts`, `gitops_analyze.task.ts`, `gitops_plan.task.ts`,
 * `gitops_publish.task.ts`, `gitops_validate.task.ts`, and `gitops_run.task.ts`.
 *
 * Accepts `repos_ops`, `git_ops`, and `npm_ops` to support testing via the
 * operations pattern (see `operations.ts` for dependency injection details).
 *
 * @module
 */

import { TaskError } from '@fuzdev/gro';
import { styleText as st } from 'node:util';
import { resolve } from 'node:path';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { to_error_message } from '@fuzdev/fuz_util/error.ts';

import { load_gitops_config, type GitopsConfig } from './gitops_config.ts';
import {
	local_repos_load,
	local_repos_resolve,
	type LocalRepo,
	type LocalRepoPath
} from './local_repo.ts';
import { load_repos_status } from './repos_status_load.ts';
import type { ReposStatusReport } from './repos_status.ts';
import type { GitOperations, NpmOperations, ReposOperations } from './operations.ts';
import { default_repos_operations } from './operations_defaults.ts';

export interface ResolveGitopsReposOptions {
	/** Path to the gitops config, absolute or relative to the cwd. */
	config: string;
	/** A `repos.toml` to use instead of the one `repos` finds walking up from the cwd. */
	registry?: string;
	/**
	 * The package whose generated data the run writes; when it's public, a
	 * private repo in the config fails the resolve.
	 */
	host?: { name: string; private: boolean };
	log?: Logger;
	repos_ops?: ReposOperations;
}

/**
 * Resolves the gitops config's repos through `repos status`: loads the
 * config's registry keys, reports on them, and resolves each to its checkout,
 * in config order. Reads nothing but git state and writes nothing.
 *
 * @returns the config, the `repos status` report, and each repo's path and entry
 * @throws {TaskError} if the config is missing, invalid, or lists no repos, `repos status` fails, or any configured repo is unknown, a reference, missing, not a repo, unprobed, or private under a public `host`
 */
export const resolve_gitops_repos = async (
	options: ResolveGitopsReposOptions
): Promise<{
	config_path: string;
	gitops_config: GitopsConfig;
	report: ReposStatusReport;
	local_repo_paths: Array<LocalRepoPath>;
}> => {
	const { config, registry, host, log, repos_ops = default_repos_operations } = options;
	const config_path = resolve(config);
	const gitops_config = await import_gitops_config(config_path);
	const keys = gitops_config.repos;
	if (keys.length === 0) {
		throw new TaskError(`No repos are configured in ${config_path}`);
	}

	log?.info(`reading the state of ${keys.length} repos from \`repos status\``);
	log?.debug('repos status targets', keys);
	const loaded = await load_repos_status({ keys, registry, repos_ops });
	if (!loaded.ok) {
		// an unknown key is the config's to fix, so name the config
		throw new TaskError(
			loaded.error?.kind === 'unknown_entry' ? `${config_path}: ${loaded.message}` : loaded.message
		);
	}
	const { report } = loaded;

	const resolved = local_repos_resolve({ keys, report, host, registry });
	if (!resolved.ok) {
		throw new TaskError(`${config_path}: ${resolved.message}`);
	}

	return { config_path, gitops_config, report, local_repo_paths: resolved.value };
};

export interface GetGitopsReadyOptions extends ResolveGitopsReposOptions {
	git_ops?: GitOperations;
	npm_ops?: NpmOperations;
	parallel?: boolean;
	concurrency?: number;
	/**
	 * Sync each repo's working tree to its entry's branch before loading
	 * (switch branch, pull, install). When `false`, repos load exactly as they
	 * sit on disk — the safe default for read-only diagnostics. Defaults to `true`.
	 */
	sync?: boolean;
	/** When syncing, tolerate uncommitted changes instead of throwing. Defaults to `false`. */
	allow_dirty?: boolean;
}

/**
 * Central initialization function for the gitops tasks that load libraries.
 *
 * Initialization sequence:
 * 1. Resolves the config's repos through `repos status` (`resolve_gitops_repos`)
 * 2. If `sync`, switches branches and pulls latest changes (in parallel by default)
 * 3. If `sync`, auto-installs deps if `package.json` changed during pull
 * 4. Loads each repo's library
 *
 * With `sync: false` (the default for read-only diagnostics), steps 2-3 are
 * skipped and repos are loaded exactly as checked out — no branch switch, pull,
 * install, or clean-workspace check.
 *
 * @param options.git_ops - for testing (defaults to real git operations)
 * @param options.npm_ops - for testing (defaults to real npm operations)
 * @param options.repos_ops - for testing (defaults to running the `repos` binary)
 * @param options.parallel - whether to load repos in parallel (default: true)
 * @param options.concurrency - max concurrent repo loads (default: 5)
 * @param options.sync - sync working trees before loading (default: true)
 * @param options.allow_dirty - when syncing, tolerate uncommitted changes (default: false)
 * @returns initialized config and fully loaded repos ready for operations
 * @throws {TaskError} if resolving the repos or loading them fails
 */
export const get_gitops_ready = async (
	options: GetGitopsReadyOptions
): Promise<{
	config_path: string;
	gitops_config: GitopsConfig;
	local_repos: Array<LocalRepo>;
}> => {
	const { log, git_ops, npm_ops, parallel, concurrency, sync, allow_dirty } = options;
	const { config_path, gitops_config, local_repo_paths } = await resolve_gitops_repos(options);

	const local_repos = await local_repos_load({
		local_repo_paths,
		log,
		git_ops,
		npm_ops,
		parallel,
		concurrency,
		sync,
		allow_dirty
	});

	return { config_path, gitops_config, local_repos };
};

export const import_gitops_config = async (config_path: string): Promise<GitopsConfig> => {
	let gitops_config: GitopsConfig | null;
	try {
		gitops_config = await load_gitops_config(config_path);
	} catch (err) {
		// an invalid config is the user's to fix, not an unexpected task failure
		throw new TaskError(to_error_message(err));
	}
	if (!gitops_config) {
		throw new TaskError(st('red', `No gitops config found at ${config_path}`));
	}
	return gitops_config;
};
