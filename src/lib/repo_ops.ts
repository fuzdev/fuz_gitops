/**
 * Generic repository operations for scripts that work across repos.
 *
 * Provides lightweight utilities for:
 * - Getting repo paths from gitops config (without full git sync)
 * - Walking files in repos with sensible exclusions
 * - Common exclusion patterns for node/svelte projects
 *
 * For full git sync/clone functionality, use `get_gitops_ready()` from gitops_task_helpers.
 */

import {existsSync} from 'node:fs';
import {readdir, stat} from 'node:fs/promises';
import {join, resolve, dirname} from 'node:path';

import {load_gitops_config} from './gitops_config.js';
import {DEFAULT_REPOS_DIR} from './paths.js';
import {GITOPS_CONFIG_PATH_DEFAULT} from './gitops_constants.js';

/** Default directories to exclude from file walking */
export const DEFAULT_EXCLUDE_DIRS = [
	'node_modules',
	'.git',
	'.gro',
	'.svelte-kit',
	'.deno',
	'.vscode',
	'.idea',
	'dist',
	'build',
	'coverage',
	'.cache',
	'.turbo',
] as const;

/** Default binary/non-text extensions to exclude from content processing */
export const DEFAULT_EXCLUDE_EXTENSIONS = [
	'.png',
	'.jpg',
	'.jpeg',
	'.gif',
	'.svg',
	'.ico',
	'.webp',
	'.woff',
	'.woff2',
	'.ttf',
	'.eot',
	'.mp4',
	'.webm',
	'.mp3',
	'.wav',
	'.ogg',
	'.zip',
	'.tar',
	'.gz',
	'.lock',
	'.pdf',
] as const;

export interface WalkOptions {
	/** Additional directories to exclude (merged with defaults) */
	exclude_dirs?: Array<string>;
	/** Additional extensions to exclude (merged with defaults) */
	exclude_extensions?: Array<string>;
	/** Maximum file size in bytes (default: 10MB) */
	max_file_size?: number;
	/** Include directories in output (default: false) */
	include_dirs?: boolean;
	/** Use only provided exclusions, ignoring defaults */
	no_defaults?: boolean;
}

export interface RepoPath {
	name: string;
	path: string;
	url: string;
}

/**
 * Get repo paths from gitops config without full git sync.
 * Lighter weight than `get_gitops_ready()` - just resolves paths.
 *
 * @param config_path Path to gitops.config.ts (defaults to ./gitops.config.ts)
 * @returns Array of repo info with name, path, and url
 */
export const get_repo_paths = async (config_path?: string): Promise<Array<RepoPath>> => {
	const resolved_config_path = resolve(config_path ?? GITOPS_CONFIG_PATH_DEFAULT);
	const config = await load_gitops_config(resolved_config_path);

	if (!config) {
		throw new Error(`No gitops config found at ${resolved_config_path}`);
	}

	const config_dir = dirname(resolved_config_path);
	const repos_dir = resolve(config_dir, config.repos_dir || DEFAULT_REPOS_DIR);

	const repos: Array<RepoPath> = [];

	for (const repo_config of config.repos) {
		const url = repo_config.repo_url;
		const name = url.split('/').at(-1);
		if (!name) continue;

		const path = repo_config.repo_dir
			? resolve(config_dir, repo_config.repo_dir)
			: join(repos_dir, name);

		if (existsSync(path)) {
			repos.push({name, path, url});
		}
	}

	return repos;
};

/**
 * Check if a path should be excluded based on options.
 */
export const should_exclude_path = (file_path: string, options?: WalkOptions): boolean => {
	const exclude_dirs = options?.no_defaults
		? (options.exclude_dirs ?? [])
		: [...DEFAULT_EXCLUDE_DIRS, ...(options?.exclude_dirs ?? [])];

	const exclude_extensions = options?.no_defaults
		? (options.exclude_extensions ?? [])
		: [...DEFAULT_EXCLUDE_EXTENSIONS, ...(options?.exclude_extensions ?? [])];

	const normalized = file_path.toLowerCase();

	// Check excluded directories
	for (const dir of exclude_dirs) {
		// Must match as a full directory component, not a prefix
		if (normalized.includes(`/${dir}/`) || normalized.endsWith(`/${dir}`)) {
			return true;
		}
	}

	// Check excluded extensions
	for (const ext of exclude_extensions) {
		if (normalized.endsWith(ext)) {
			return true;
		}
	}

	return false;
};

/**
 * Walk files in a directory, respecting common exclusions.
 * Yields absolute paths to files (and optionally directories).
 *
 * @param dir Directory to walk
 * @param options Walk options for exclusions and filtering
 */
export async function* walk_repo_files(
	dir: string,
	options?: WalkOptions,
): AsyncGenerator<string, void, undefined> {
	const max_file_size = options?.max_file_size ?? 10 * 1024 * 1024;
	const include_dirs = options?.include_dirs ?? false;

	async function* walk(current_dir: string): AsyncGenerator<string, void, undefined> {
		let entries;
		try {
			entries = await readdir(current_dir, {withFileTypes: true});
		} catch {
			// Skip directories we can't read
			return;
		}

		for (const entry of entries) {
			const full_path = join(current_dir, entry.name);

			if (should_exclude_path(full_path, options)) {
				continue;
			}

			if (entry.isDirectory()) {
				if (include_dirs) {
					yield full_path;
				}
				yield* walk(full_path);
			} else if (entry.isFile()) {
				// Check file size
				try {
					const file_stat = await stat(full_path); // eslint-disable-line no-await-in-loop
					if (file_stat.size <= max_file_size) {
						yield full_path;
					}
				} catch {
					// Skip files we can't stat
				}
			}
		}
	}

	yield* walk(dir);
}

/**
 * Collect all files from walk_repo_files into an array.
 * Convenience function for when you need all paths upfront.
 */
export const collect_repo_files = async (
	dir: string,
	options?: WalkOptions,
): Promise<Array<string>> => {
	const files: Array<string> = [];
	for await (const file of walk_repo_files(dir, options)) {
		files.push(file);
	}
	return files;
};
