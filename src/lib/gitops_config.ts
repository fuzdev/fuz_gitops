/**
 * Configuration types and normalization for gitops multi-repo management.
 *
 * Two-phase configuration system:
 * - `RawGitopsConfig` - User-friendly format with optional fields and flexible types
 * - `GitopsConfig` - Internal format with required fields and strict types
 *
 * This allows users to provide minimal configs (e.g., just URLs as strings) while
 * the system works with normalized configs internally for type safety.
 */

import type {Url} from '@fuzdev/fuz_util/url.js';
import {existsSync} from 'node:fs';
import {strip_end} from '@fuzdev/fuz_util/string.js';
import type {GitBranch} from '@fuzdev/fuz_util/git.js';

import {DEFAULT_REPOS_DIR} from './paths.js';

export interface GitopsConfig {
	repos: Array<GitopsRepoConfig>;
	repos_dir: string;
}

export type CreateGitopsConfig = (
	base_config: GitopsConfig,
) => RawGitopsConfig | Promise<RawGitopsConfig>;

export interface RawGitopsConfig {
	repos?: Array<Url | RawGitopsRepoConfig>;
	repos_dir?: string;
}

export interface GitopsRepoConfig {
	/**
	 * The HTTPS URL to the repo. Does not include a `.git` suffix.
	 * @example 'https://github.com/fuzdev/fuz_ui'
	 */
	repo_url: Url;

	/**
	 * Relative or absolute path to the repo's local directory.
	 * If `null`, the directory is inferred from the URL and cwd.
	 * @example 'relative/path/to/repo'
	 * @example '/absolute/path/to/repo'
	 */
	repo_dir: string | null;

	/**
	 * The branch name to use when fetching the repo. Defaults to `main`.
	 */
	branch: GitBranch;
}

export interface RawGitopsRepoConfig {
	repo_url: Url;
	repo_dir?: string | null;
	branch?: GitBranch;
}

export const create_empty_gitops_config = (): GitopsConfig => ({
	repos: [],
	repos_dir: DEFAULT_REPOS_DIR,
});

/**
 * Transforms a `RawGitopsConfig` to the more strict `GitopsConfig`.
 * This allows users to provide a more relaxed config.
 */
export const normalize_gitops_config = (raw_config: RawGitopsConfig): GitopsConfig => {
	const empty_config = create_empty_gitops_config();
	// All of the raw config properties are optional,
	// so fall back to the empty values when `undefined`.
	const {repos, repos_dir} = raw_config;
	return {
		repos: repos ? repos.map((r) => parse_fuz_repo_config(r)) : empty_config.repos,
		// Default to two dirs up from config if not specified
		repos_dir: repos_dir ?? DEFAULT_REPOS_DIR,
	};
};

const parse_fuz_repo_config = (r: Url | RawGitopsRepoConfig): GitopsRepoConfig => {
	if (typeof r === 'string') {
		return {repo_url: r, repo_dir: null, branch: 'main' as GitBranch}; // TODO @zts use flavored for GitBranch
	}
	return {
		repo_url: strip_end(r.repo_url, '.git'),
		repo_dir: r.repo_dir ?? null,
		branch: r.branch ?? ('main' as GitBranch), // TODO @zts use flavored for GitBranch
	};
};

export interface GitopsConfigModule {
	readonly default: RawGitopsConfig | CreateGitopsConfig;
}

export const load_gitops_config = async (config_path: string): Promise<GitopsConfig | null> => {
	if (!existsSync(config_path)) {
		// No user config file found.
		return null;
	}
	// Import the user's `gitops.config.ts`.
	const config_module = await import(config_path);
	validate_gitops_config_module(config_module, config_path);
	return normalize_gitops_config(
		typeof config_module.default === 'function'
			? await config_module.default(create_empty_gitops_config())
			: config_module.default,
	);
};

export const validate_gitops_config_module: (
	config_module: any,
	config_path: string,
) => asserts config_module is GitopsConfigModule = (config_module, config_path) => {
	const config = config_module.default;
	if (!config) {
		throw Error(`Invalid Fuz config module at ${config_path}: expected a default export`);
	} else if (!(typeof config === 'function' || typeof config === 'object')) {
		throw Error(
			`Invalid Fuz config module at ${config_path}: the default export must be a function or object`,
		);
	}
};
