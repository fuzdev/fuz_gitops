/**
 * Shared initialization logic for all gitops tasks.
 *
 * Provides `get_gitops_ready()` which orchestrates:
 * - Config loading and normalization
 * - Repo resolution (local path discovery)
 * - Branch switching and syncing
 * - Dependency installation
 *
 * Used by: `gitops_sync.task.ts`, `gitops_analyze.task.ts`, `gitops_plan.task.ts`,
 * `gitops_publish.task.ts`, and `gitops_validate.task.ts`.
 *
 * Accepts `git_ops` and `npm_ops` parameters to support testing via operations pattern
 * (see `operations.ts` for dependency injection details).
 *
 * @module
 */

import {TaskError} from '@ryanatkn/gro';
import {styleText as st} from 'node:util';
import {resolve, dirname} from 'node:path';
import {print_path} from '@ryanatkn/gro/paths.js';
import type {Logger} from '@fuzdev/fuz_util/log.js';

import {load_gitops_config, type GitopsConfig} from './gitops_config.js';
import {local_repos_load, local_repos_ensure, type LocalRepo} from './local_repo.js';
import {resolve_gitops_config} from './resolved_gitops_config.js';
import {DEFAULT_REPOS_DIR} from './paths.js';
import type {GitOperations, NpmOperations} from './operations.js';

export interface GetGitopsReadyOptions {
	config: string;
	dir?: string;
	download: boolean;
	log?: Logger;
	git_ops?: GitOperations;
	npm_ops?: NpmOperations;
	parallel?: boolean;
	concurrency?: number;
}

/**
 * Central initialization function for all gitops tasks.
 *
 * Initialization sequence:
 * 1. Loads and normalizes config from `gitops.config.ts`
 * 2. Resolves local repo paths (creates missing with `--download`)
 * 3. Switches branches and pulls latest changes (in parallel by default)
 * 4. Auto-installs deps if package.json changed during pull
 *
 * Priority for path resolution:
 * - `dir` argument (explicit override)
 * - Config `repos_dir` setting
 * - `DEFAULT_REPOS_DIR` constant
 *
 * @param options.git_ops for testing (defaults to real git operations)
 * @param options.npm_ops for testing (defaults to real npm operations)
 * @param options.parallel whether to load repos in parallel (default: true)
 * @param options.concurrency max concurrent repo loads (default: 5)
 * @returns initialized config and fully loaded repos ready for operations
 * @throws {TaskError} if config loading or repo resolution fails
 */
export const get_gitops_ready = async (
	options: GetGitopsReadyOptions,
): Promise<{
	config_path: string;
	repos_dir: string;
	gitops_config: GitopsConfig;
	local_repos: Array<LocalRepo>;
}> => {
	const {config, dir, download, log, git_ops, npm_ops, parallel, concurrency} = options;
	const config_path = resolve(config);
	const gitops_config = await import_gitops_config(config_path);

	// Priority: explicit dir arg → config repos_dir → default (two dirs up from config)
	const repos_dir = resolve_gitops_paths({
		config,
		dir,
		config_repos_dir: gitops_config.repos_dir,
	}).repos_dir;

	log?.info(
		`resolving gitops configs on the filesystem in ${repos_dir}`,
		gitops_config.repos.map((r) => r.repo_url),
	);
	const resolved_config = resolve_gitops_config(gitops_config, repos_dir);

	const local_repo_paths = await local_repos_ensure({
		resolved_config,
		repos_dir,
		gitops_config,
		download,
		log,
		npm_ops,
	});

	const local_repos = await local_repos_load({
		local_repo_paths,
		log,
		git_ops,
		npm_ops,
		parallel,
		concurrency,
	});

	return {config_path, repos_dir, gitops_config, local_repos};
};

export interface ResolveGitopsPathsOptions {
	config: string;
	dir?: string;
	config_repos_dir?: string;
}

export const resolve_gitops_paths = (
	options: ResolveGitopsPathsOptions,
): {config_path: string; repos_dir: string} => {
	const {config, dir, config_repos_dir} = options;
	const config_path = resolve(config);
	const config_dir = dirname(config_path);

	// Priority: explicit dir arg → config repos_dir → default (parent of config dir)
	const repos_dir =
		dir !== undefined
			? resolve(dir)
			: config_repos_dir !== undefined
				? resolve(config_dir, config_repos_dir)
				: resolve(config_dir, DEFAULT_REPOS_DIR);

	return {config_path, repos_dir};
};

export const import_gitops_config = async (config_path: string): Promise<GitopsConfig> => {
	const gitops_config = await load_gitops_config(config_path);
	if (!gitops_config) {
		throw new TaskError(st('red', `No gitops config found at ${print_path(config_path)}`));
	}
	return gitops_config;
};
