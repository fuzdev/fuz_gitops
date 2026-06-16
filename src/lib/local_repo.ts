import {strip_end} from '@fuzdev/fuz_util/string.ts';
import {to_error_message} from '@fuzdev/fuz_util/error.ts';
import type {LibraryJson} from '@fuzdev/fuz_util/library_json.ts';
import type {PackageJson} from '@fuzdev/fuz_util/package_json.ts';
import {Library} from '@fuzdev/fuz_ui/library.svelte.ts';
import {existsSync} from 'node:fs';
import {join} from 'node:path';
import {TaskError} from '@fuzdev/gro';
import {library_load_from_repo} from '@fuzdev/gro/library_load.ts';
import type {Logger} from '@fuzdev/fuz_util/log.ts';
import {spawn_out} from '@fuzdev/fuz_util/process.ts';
import {map_concurrent_settled} from '@fuzdev/fuz_util/async.ts';
import type {GitOperations, NpmOperations} from './operations.ts';
import {default_git_operations, default_npm_operations} from './operations_defaults.ts';

import type {GitopsConfig, GitopsRepoConfig} from './gitops_config.ts';
import type {ResolvedGitopsConfig} from './resolved_gitops_config.ts';
import {GITOPS_CONCURRENCY_DEFAULT} from './gitops_constants.ts';

/**
 * Fully loaded local repo with `Library` and extracted dependency data.
 * Does not extend `LocalRepoPath` - `Library` is source of truth for name/repo_url/etc.
 */
export interface LocalRepo {
	library: Library;
	/** The repo's full `package.json` (with `dependencies`/`devDependencies`). */
	package_json: PackageJson;
	repo_dir: string;
	repo_git_ssh_url: string;
	repo_config: GitopsRepoConfig;
	dependencies?: Map<string, string>;
	dev_dependencies?: Map<string, string>;
	peer_dependencies?: Map<string, string>;
}

/**
 * A repo that has been located on the filesystem (path exists).
 * Used before loading - just filesystem/git concerns.
 */
export interface LocalRepoPath {
	type: 'local_repo_path';
	repo_name: string; // from URL parsing (for display/logging before Library loaded)
	repo_dir: string;
	repo_url: string;
	repo_git_ssh_url: string;
	repo_config: GitopsRepoConfig;
}

/**
 * A repo that is missing from the filesystem (needs cloning).
 */
export interface LocalRepoMissing {
	type: 'local_repo_missing';
	repo_name: string;
	repo_url: string;
	repo_git_ssh_url: string;
	repo_config: GitopsRepoConfig;
}

/**
 * Loads repo data, optionally syncing the working tree first.
 *
 * When `sync` is `false` (the default for read-only diagnostics like
 * `gitops_analyze`/`gitops_plan`), the repo is loaded exactly as it sits on
 * disk — no branch switch, pull, install, or clean-workspace check. This makes
 * those commands safe to run on an active workspace with uncommitted changes or
 * feature branches checked out.
 *
 * When `sync` is `true` (used by `gitops_sync`), the working tree is brought in
 * line with the configured branch first:
 * 1. Records current commit hash (for detecting changes)
 * 2. Switches to target branch if needed (requires clean workspace unless `allow_dirty`)
 * 3. Pulls latest changes from remote (skipped for local-only repos)
 * 4. Validates workspace is clean after pull (skipped if `allow_dirty`)
 * 5. Auto-installs dependencies if `package.json` changed
 *
 * Either way it then:
 * 6. Loads `library_json` via `library_load_from_repo` (svelte-docinfo analysis)
 * 7. Creates `Library` and extracts dependency maps
 *
 * @param sync - sync the working tree to the configured branch before loading (default `true`)
 * @param allow_dirty - when syncing, tolerate uncommitted changes instead of throwing (default `false`)
 * @throws {TaskError} if syncing fails (dirty workspace, branch switch, install) or analysis fails
 */
export const local_repo_load = async ({
	local_repo_path,
	log: _log,
	git_ops = default_git_operations,
	npm_ops = default_npm_operations,
	sync = true,
	allow_dirty = false,
}: {
	local_repo_path: LocalRepoPath;
	log?: Logger;
	git_ops?: GitOperations;
	npm_ops?: NpmOperations;
	sync?: boolean;
	allow_dirty?: boolean;
}): Promise<LocalRepo> => {
	const {repo_config, repo_dir, repo_name, repo_git_ssh_url} = local_repo_path;

	if (sync) {
		// Record commit hash before any changes
		const commit_before_result = await git_ops.current_commit_hash({cwd: repo_dir});
		if (!commit_before_result.ok) {
			throw new TaskError(
				`Failed to get commit hash in ${repo_dir}: ${commit_before_result.message}`,
			);
		}
		const commit_before = commit_before_result.value;

		// Switch to target branch if needed
		const branch_result = await git_ops.current_branch_name({cwd: repo_dir});
		if (!branch_result.ok) {
			throw new TaskError(`Failed to get current branch in ${repo_dir}: ${branch_result.message}`);
		}

		const switched_branches = branch_result.value !== repo_config.branch;
		if (switched_branches) {
			// Guard the switch on a clean workspace unless the caller opts into `allow_dirty`,
			// in which case we let `git checkout` itself fail loudly if it can't proceed.
			if (!allow_dirty) {
				const clean_result = await git_ops.check_clean_workspace({cwd: repo_dir});
				if (!clean_result.ok) {
					throw new TaskError(`Failed to check workspace in ${repo_dir}: ${clean_result.message}`);
				}

				if (!clean_result.value) {
					throw new TaskError(
						`Repo ${repo_dir} is not on branch "${repo_config.branch}" and the workspace is unclean, blocking switch`,
					);
				}
			}

			const checkout_result = await git_ops.checkout({branch: repo_config.branch, cwd: repo_dir});
			if (!checkout_result.ok) {
				throw new TaskError(
					`Failed to checkout branch "${repo_config.branch}" in ${repo_dir}: ${checkout_result.message}`,
				);
			}
		}

		// Only pull if remote exists (skip for local-only repos, test fixtures)
		const origin_result = await git_ops.has_remote({remote: 'origin', cwd: repo_dir});
		if (!origin_result.ok) {
			throw new TaskError(`Failed to check for remote in ${repo_dir}: ${origin_result.message}`);
		}

		if (origin_result.value) {
			// Pull the configured branch explicitly. Without a branch, `git pull origin`
			// targets the remote's default branch (origin/HEAD), which for a repo checked
			// out on a non-default branch rebases the wrong branch onto it.
			const pull_result = await git_ops.pull({branch: repo_config.branch, cwd: repo_dir});
			if (!pull_result.ok) {
				throw new TaskError(`Failed to pull in ${repo_dir}: ${pull_result.message}`);
			}
		}

		// Check clean workspace after pull to ensure we're in a good state
		// (skipped when `allow_dirty`, since uncommitted changes are expected then)
		if (!allow_dirty) {
			const clean_after_result = await git_ops.check_clean_workspace({cwd: repo_dir});
			if (!clean_after_result.ok) {
				throw new TaskError(
					`Failed to check workspace in ${repo_dir}: ${clean_after_result.message}`,
				);
			}

			if (!clean_after_result.value) {
				throw new TaskError(
					`Workspace ${repo_dir} is unclean after pulling branch "${repo_config.branch}"`,
				);
			}
		}

		// Record commit hash after pull
		const commit_after_result = await git_ops.current_commit_hash({cwd: repo_dir});
		if (!commit_after_result.ok) {
			throw new TaskError(
				`Failed to get commit hash in ${repo_dir}: ${commit_after_result.message}`,
			);
		}
		const commit_after = commit_after_result.value;

		// Track if we got new commits
		const got_new_commits = commit_before !== commit_after;

		// Only install if package.json changed
		if (got_new_commits) {
			const changed_result = await git_ops.has_file_changed({
				from_commit: commit_before,
				to_commit: commit_after,
				file_path: 'package.json',
				cwd: repo_dir,
			});

			if (!changed_result.ok) {
				throw new TaskError(
					`Failed to check if package.json changed in ${repo_dir}: ${changed_result.message}`,
				);
			}

			if (changed_result.value) {
				const install_result = await npm_ops.install({cwd: repo_dir});
				if (!install_result.ok) {
					throw new TaskError(
						`Failed to install dependencies in ${repo_dir}: ${install_result.message}${install_result.stderr ? `\n${install_result.stderr}` : ''}`,
					);
				}
			}
		}
	}

	// Load library metadata via svelte-docinfo analysis (cached under `.gro/library.json`).
	let library_json: LibraryJson;
	let package_json: PackageJson;
	try {
		({library_json, package_json} = await library_load_from_repo(repo_dir, {log: _log}));
	} catch (err) {
		const message = to_error_message(err);
		_log?.warn(
			`Failed to load library metadata for repo "${repo_name}" in ${repo_dir}: ${message}`,
		);
		throw new TaskError(
			`Failed to load library metadata for repo "${repo_name}" in ${repo_dir}: ${message}`,
		);
	}
	const library = new Library(library_json);

	const local_repo: LocalRepo = {
		library,
		package_json,
		repo_dir,
		repo_git_ssh_url,
		repo_config,
	};

	// Extract dependencies from the full package_json
	if (package_json.dependencies) {
		local_repo.dependencies = new Map(Object.entries(package_json.dependencies));
	}
	if (package_json.devDependencies) {
		local_repo.dev_dependencies = new Map(Object.entries(package_json.devDependencies));
	}
	if (package_json.peerDependencies) {
		local_repo.peer_dependencies = new Map(Object.entries(package_json.peerDependencies));
	}

	return local_repo;
};

export const local_repos_ensure = async ({
	resolved_config,
	repos_dir,
	gitops_config,
	download,
	log,
	npm_ops = default_npm_operations,
}: {
	resolved_config: ResolvedGitopsConfig;
	repos_dir: string;
	gitops_config: GitopsConfig;
	download: boolean;
	log?: Logger;
	npm_ops?: NpmOperations;
}): Promise<Array<LocalRepoPath>> => {
	let local_repo_paths: Array<LocalRepoPath> | null = null;

	if (!resolved_config.local_repos_missing) {
		local_repo_paths = resolved_config.local_repo_paths;
	} else {
		if (download) {
			const downloaded = await download_repos({
				repos_dir,
				local_repos_missing: resolved_config.local_repos_missing,
				log,
				npm_ops,
			});
			local_repo_paths = (resolved_config.local_repo_paths ?? [])
				.concat(downloaded)
				.sort(
					(a, b) =>
						gitops_config.repos.findIndex((r) => r.repo_url === a.repo_url) -
						gitops_config.repos.findIndex((r) => r.repo_url === b.repo_url),
				);
		} else {
			log?.error(
				`Failed to resolve local repos in ${repos_dir} - do you need to pass \`--download\` or configure the directory?`, // TODO leaking task impl details
				resolved_config.local_repos_missing.map((r) => r.repo_url),
			);
			throw new TaskError('Failed to resolve local configs');
		}
	}

	if (!local_repo_paths) {
		throw new TaskError('No repos are configured in `gitops_config.ts`');
	}

	return local_repo_paths;
};

export const local_repos_load = async ({
	local_repo_paths,
	log,
	git_ops = default_git_operations,
	npm_ops = default_npm_operations,
	parallel = true,
	concurrency = GITOPS_CONCURRENCY_DEFAULT,
	sync = true,
	allow_dirty = false,
}: {
	local_repo_paths: Array<LocalRepoPath>;
	log?: Logger;
	git_ops?: GitOperations;
	npm_ops?: NpmOperations;
	parallel?: boolean;
	concurrency?: number;
	sync?: boolean;
	allow_dirty?: boolean;
}): Promise<Array<LocalRepo>> => {
	if (!parallel) {
		// Sequential loading (original behavior)
		const loaded: Array<LocalRepo> = [];
		for (const local_repo_path of local_repo_paths) {
			loaded.push(
				await local_repo_load({local_repo_path, log, git_ops, npm_ops, sync, allow_dirty}),
			);
		}
		return loaded;
	}

	// Parallel loading with concurrency limit
	const results = await map_concurrent_settled(
		local_repo_paths,
		concurrency,
		async (local_repo_path) => {
			return local_repo_load({local_repo_path, log, git_ops, npm_ops, sync, allow_dirty});
		},
	);

	// Check for failures and collect successes
	const loaded: Array<LocalRepo> = [];
	const errors: Array<{repo_name: string; error: string}> = [];

	for (let i = 0; i < results.length; i++) {
		const result = results[i]!;
		if (result.status === 'fulfilled') {
			loaded.push(result.value);
		} else {
			const repo_path = local_repo_paths[i]!;
			errors.push({
				repo_name: repo_path.repo_name,
				error: String(result.reason),
			});
		}
	}

	// If any repos failed to load, throw with details
	if (errors.length > 0) {
		const error_details = errors.map((e) => `  ${e.repo_name}: ${e.error}`).join('\n');
		throw new TaskError(`Failed to load ${errors.length} repos:\n${error_details}`);
	}

	return loaded;
};

export const local_repo_locate = ({
	repo_config,
	repos_dir,
}: {
	repo_config: GitopsRepoConfig;
	repos_dir: string;
}): LocalRepoPath | LocalRepoMissing => {
	const {repo_url} = repo_config;
	const repo_name = strip_end(repo_url, '/').split('/').at(-1);
	if (!repo_name) throw Error('Invalid `repo_config.repo_url` ' + repo_url);

	const repo_git_ssh_url = to_repo_git_ssh_url(repo_url);

	const repo_dir = repo_config.repo_dir ?? join(repos_dir, repo_name);
	if (!existsSync(repo_dir)) {
		return {type: 'local_repo_missing', repo_name, repo_url, repo_git_ssh_url, repo_config};
	}

	return {
		type: 'local_repo_path',
		repo_name,
		repo_dir,
		repo_url,
		repo_git_ssh_url,
		repo_config,
	};
};

const to_repo_git_ssh_url = (repo_url: string): string => {
	const url = new URL(repo_url);
	return `git@${url.hostname}:${url.pathname.substring(1)}`;
};

const download_repos = async ({
	repos_dir,
	local_repos_missing,
	log,
	npm_ops = default_npm_operations,
}: {
	repos_dir: string;
	local_repos_missing: Array<LocalRepoMissing>;
	log?: Logger;
	npm_ops?: NpmOperations;
}): Promise<Array<LocalRepoPath>> => {
	const resolved: Array<LocalRepoPath> = [];
	for (const {repo_config, repo_git_ssh_url} of local_repos_missing) {
		log?.info(`cloning repo ${repo_git_ssh_url} to ${repos_dir}`);
		const clone_result = await spawn_out('git', ['clone', repo_git_ssh_url], {cwd: repos_dir});
		if (!clone_result.result.ok) {
			throw new TaskError(
				`Failed to clone repo ${repo_git_ssh_url} to ${repos_dir}${clone_result.stderr ? ': ' + clone_result.stderr.trim() : ''}`,
			);
		}
		const local_repo = local_repo_locate({repo_config, repos_dir});
		if (local_repo.type === 'local_repo_missing') {
			throw new TaskError(
				`Failed to clone repo ${repo_git_ssh_url} to ${repos_dir}: directory not found after clone`,
			);
		}
		// Always install dependencies after cloning
		log?.info(`installing dependencies for newly cloned repo ${local_repo.repo_dir}`);
		const install_result = await npm_ops.install({cwd: local_repo.repo_dir});
		if (!install_result.ok) {
			throw new TaskError(
				`Failed to install dependencies in ${local_repo.repo_dir}: ${install_result.message}${install_result.stderr ? `\n${install_result.stderr}` : ''}`,
			);
		}
		resolved.push(local_repo);
	}
	return resolved;
};
