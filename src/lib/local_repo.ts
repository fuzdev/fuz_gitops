import { to_error_message } from '@fuzdev/fuz_util/error.ts';
import { library_json_from_modules, type LibraryJson } from '@fuzdev/fuz_util/library_json.ts';
import type { PackageJson } from '@fuzdev/fuz_util/package_json.ts';
import type { Result } from '@fuzdev/fuz_util/result.ts';
import { Library } from '@fuzdev/fuz_ui/library.svelte.ts';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { TaskError } from '@fuzdev/gro';
import { library_load_from_repo } from '@fuzdev/gro/library_load.ts';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { map_concurrent_settled } from '@fuzdev/fuz_util/async.ts';
import type { GitOperations, NpmOperations } from './operations.ts';
import { default_git_operations, default_npm_operations } from './operations_defaults.ts';

import { gitops_config_leaked_private_repos } from './gitops_config.ts';
import type { ReposEntryStatus, ReposStatusReport } from './repos_status.ts';
import { GITOPS_CONCURRENCY_DEFAULT } from './gitops_constants.ts';
import { cargo_toml_load } from './cargo_toml.ts';

/**
 * Fully loaded local repo with `Library` and extracted dependency data.
 * Does not extend `LocalRepoPath` - `Library` is source of truth for name/repo_url/etc.
 */
export interface LocalRepo {
	/**
	 * Which packaging ecosystem the repo belongs to. `npm` repos (with a
	 * `package.json`) take part in the changeset publishing cascade; `cargo` repos
	 * (a Rust `Cargo.toml`, no `package.json`) are dashboard-only — fetched and
	 * rendered like any repo but excluded from publishing/analysis. See
	 * `repo_is_npm`.
	 */
	kind: 'npm' | 'cargo';
	library: Library;
	/** The repo's full `package.json` (with `dependencies`/`devDependencies`). */
	package_json: PackageJson;
	repo_dir: string;
	/**
	 * The repo's registry entry as `repos status --json` reported it: its
	 * `branch`, `visibility`, `ci`, and `archived`, and its git state.
	 */
	entry: ReposEntryStatus;
	dependencies?: Map<string, string>;
	dev_dependencies?: Map<string, string>;
	peer_dependencies?: Map<string, string>;
}

/**
 * A configured repo resolved through `repos status`: present on disk, before
 * its library is loaded. See `local_repos_resolve`.
 */
export interface LocalRepoPath {
	/** The repo's registry key (for display/logging before `Library` is loaded). */
	repo_name: string;
	/** The workspace root joined with the entry's `dir`. */
	repo_dir: string;
	/** The registry's HTTPS URL for the repo. */
	repo_url: string;
	/** The repo's registry entry as `repos status --json` reported it. */
	entry: ReposEntryStatus;
}

/**
 * Resolves a gitops config's registry keys against a `repos status` report,
 * in config order. Every key must name an owned repo that's present and was
 * probed; each that doesn't is a problem, and any problem fails the whole
 * resolve, naming them all.
 *
 * @param options.keys - the config's registry keys, in config order
 * @param options.report - `repos status <keys…> --json`'s report
 * @param options.host - the package whose generated data the run writes; when public, a private repo is a problem (`gitops_config_leaked_private_repos`)
 * @param options.registry - the `--registry` the report was made with, repeated in the hints
 * @returns the repos in config order, or a message listing every problem
 */
export const local_repos_resolve = (options: {
	keys: ReadonlyArray<string>;
	report: ReposStatusReport;
	host?: { name: string; private: boolean };
	registry?: string;
}): Result<{ value: Array<LocalRepoPath> }, { message: string; problems: Array<string> }> => {
	const { keys, report, host, registry } = options;
	const repos_command = registry === undefined ? 'repos' : `repos --registry ${registry}`;
	const by_key = new Map(report.entries.map((e) => [e.key, e] as const));

	const problems: Array<string> = [];
	const resolved: Array<LocalRepoPath> = [];
	for (const key of keys) {
		const entry = by_key.get(key);
		if (!entry) {
			// `repos` also takes a dir name or path as a target, reporting the entry by its key
			const by_dir = report.entries.find((e) => e.dir === key);
			problems.push(
				by_dir
					? `\`${key}\` is the dir of \`${by_dir.key}\`, not a registry key — list \`${by_dir.key}\``
					: `\`${key}\` isn't in the \`repos status\` report — is it a registry key?`
			);
			continue;
		}
		const repo_dir = join(report.workspace, entry.dir);
		if (entry.kind === 'reference') {
			problems.push(`\`${key}\` is a third-party reference, not an owned repo`);
		} else if (entry.presence.kind === 'missing') {
			problems.push(
				`\`${key}\` is missing at ${repo_dir} — \`${repos_command} sync ${key}\` clones it`
			);
		} else if (entry.presence.kind === 'not_a_repo') {
			problems.push(`\`${key}\`: ${repo_dir} isn't a git repo`);
		} else if (entry.probe_error !== null) {
			problems.push(`\`${key}\`: probing ${repo_dir} failed: ${entry.probe_error}`);
		} else {
			resolved.push({ repo_name: key, repo_dir, repo_url: entry.url, entry });
		}
	}

	if (host) {
		const configured = keys.map((k) => by_key.get(k)).filter((e) => e !== undefined);
		for (const leaked of gitops_config_leaked_private_repos(configured, host.private)) {
			problems.push(
				`\`${leaked.key}\` is private, and ${host.name} is a public package whose generated repos.json would publish its metadata`
			);
		}
	}

	if (problems.length) {
		return {
			ok: false,
			message: `${problems.length === 1 ? 'a configured repo' : 'configured repos'} can't be loaded:\n  ${problems.join('\n  ')}`,
			problems
		};
	}
	return { ok: true, value: resolved };
};

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
 * line with the branch its registry entry follows first:
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
 * @param sync - sync the working tree to the entry's branch before loading (default `true`)
 * @param allow_dirty - when syncing, tolerate uncommitted changes instead of throwing (default `false`)
 * @throws {TaskError} if syncing fails (dirty workspace, branch switch, install) or analysis fails
 */
export const local_repo_load = async ({
	local_repo_path,
	log: _log,
	git_ops = default_git_operations,
	npm_ops = default_npm_operations,
	sync = true,
	allow_dirty = false
}: {
	local_repo_path: LocalRepoPath;
	log?: Logger;
	git_ops?: GitOperations;
	npm_ops?: NpmOperations;
	sync?: boolean;
	allow_dirty?: boolean;
}): Promise<LocalRepo> => {
	const { entry, repo_dir, repo_name } = local_repo_path;

	if (sync) {
		const { branch } = entry;
		if (branch === null) {
			throw new TaskError(
				`Repo ${repo_name} follows no branch in the registry, so it can't be synced`
			);
		}

		// Record commit hash before any changes
		const commit_before_result = await git_ops.current_commit_hash({ cwd: repo_dir });
		if (!commit_before_result.ok) {
			throw new TaskError(
				`Failed to get commit hash in ${repo_dir}: ${commit_before_result.message}`
			);
		}
		const commit_before = commit_before_result.value;

		// Switch to target branch if needed
		const branch_result = await git_ops.current_branch_name({ cwd: repo_dir });
		if (!branch_result.ok) {
			throw new TaskError(`Failed to get current branch in ${repo_dir}: ${branch_result.message}`);
		}

		const switched_branches = branch_result.value !== branch;
		if (switched_branches) {
			// Guard the switch on a clean workspace unless the caller opts into `allow_dirty`,
			// in which case we let `git checkout` itself fail loudly if it can't proceed.
			if (!allow_dirty) {
				const clean_result = await git_ops.check_clean_workspace({ cwd: repo_dir });
				if (!clean_result.ok) {
					throw new TaskError(`Failed to check workspace in ${repo_dir}: ${clean_result.message}`);
				}

				if (!clean_result.value) {
					throw new TaskError(
						`Repo ${repo_dir} is not on branch "${branch}" and the workspace is unclean, blocking switch`
					);
				}
			}

			const checkout_result = await git_ops.checkout({ branch, cwd: repo_dir });
			if (!checkout_result.ok) {
				throw new TaskError(
					`Failed to checkout branch "${branch}" in ${repo_dir}: ${checkout_result.message}`
				);
			}
		}

		// Only pull if remote exists (skip for local-only repos, test fixtures)
		const origin_result = await git_ops.has_remote({ remote: 'origin', cwd: repo_dir });
		if (!origin_result.ok) {
			throw new TaskError(`Failed to check for remote in ${repo_dir}: ${origin_result.message}`);
		}

		if (origin_result.value) {
			// Pull the entry's branch explicitly. Without a branch, `git pull origin`
			// targets the remote's default branch (origin/HEAD), which for a repo checked
			// out on a non-default branch rebases the wrong branch onto it.
			const pull_result = await git_ops.pull({ branch, cwd: repo_dir });
			if (!pull_result.ok) {
				throw new TaskError(`Failed to pull in ${repo_dir}: ${pull_result.message}`);
			}
		}

		// Check clean workspace after pull to ensure we're in a good state
		// (skipped when `allow_dirty`, since uncommitted changes are expected then)
		if (!allow_dirty) {
			const clean_after_result = await git_ops.check_clean_workspace({ cwd: repo_dir });
			if (!clean_after_result.ok) {
				throw new TaskError(
					`Failed to check workspace in ${repo_dir}: ${clean_after_result.message}`
				);
			}

			if (!clean_after_result.value) {
				throw new TaskError(`Workspace ${repo_dir} is unclean after pulling branch "${branch}"`);
			}
		}

		// Record commit hash after pull
		const commit_after_result = await git_ops.current_commit_hash({ cwd: repo_dir });
		if (!commit_after_result.ok) {
			throw new TaskError(
				`Failed to get commit hash in ${repo_dir}: ${commit_after_result.message}`
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
				cwd: repo_dir
			});

			if (!changed_result.ok) {
				throw new TaskError(
					`Failed to check if package.json changed in ${repo_dir}: ${changed_result.message}`
				);
			}

			if (changed_result.value) {
				const install_result = await npm_ops.install({ cwd: repo_dir });
				if (!install_result.ok) {
					throw new TaskError(
						`Failed to install dependencies in ${repo_dir}: ${install_result.message}${install_result.stderr ? `\n${install_result.stderr}` : ''}`
					);
				}
			}
		}
	}

	// A repo with no `package.json` but a Rust `Cargo.toml` isn't an npm package and can't be
	// analyzed as a library. Load it as a dashboard-only `cargo` repo (CI, PRs, identity) that
	// publishing/analysis skips. Anything else falls through to the npm loader below, whose
	// error covers a genuinely missing or unreadable manifest.
	if (!existsSync(join(repo_dir, 'package.json')) && existsSync(join(repo_dir, 'Cargo.toml'))) {
		return local_repo_load_cargo({ local_repo_path });
	}

	// Load library metadata via svelte-docinfo analysis (cached under `.gro/library.json`).
	let library_json: LibraryJson;
	let package_json: PackageJson;
	try {
		({ library_json, package_json } = await library_load_from_repo(repo_dir, { log: _log }));
	} catch (err) {
		const message = to_error_message(err);
		_log?.warn(
			`Failed to load library metadata for repo "${repo_name}" in ${repo_dir}: ${message}`
		);
		throw new TaskError(
			`Failed to load library metadata for repo "${repo_name}" in ${repo_dir}: ${message}`
		);
	}
	const library = new Library(library_json);

	const local_repo: LocalRepo = {
		kind: 'npm',
		library,
		package_json,
		repo_dir,
		entry
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

/**
 * Whether a repo is an npm package and so participates in publishing and
 * dependency analysis. Non-npm repos (e.g. Rust `cargo` repos) are still synced
 * and rendered on the dashboard, but excluded from the changeset cascade.
 */
export const repo_is_npm = (repo: LocalRepo): boolean => repo.kind === 'npm';

/**
 * Loads a non-npm Rust repo as a dashboard-only `LocalRepo`. It has no
 * `package.json`, so there's no `svelte-docinfo` analysis and no npm dependency
 * graph — a lightweight `Library` is synthesized from the repo's `Cargo.toml`
 * (best-effort name/version/description) and its registry URL, which is
 * what the dashboard renders (CI status, PRs, identity). Marked `private` so it
 * never reads as an npm publish target, and tagged `cargo` so the publishing and
 * analysis paths skip it (see `repo_is_npm`).
 */
const local_repo_load_cargo = async ({
	local_repo_path
}: {
	local_repo_path: LocalRepoPath;
}): Promise<LocalRepo> => {
	const { entry, repo_dir, repo_name, repo_url } = local_repo_path;

	const cargo = await cargo_toml_load(repo_dir);

	// A Cargo workspace root has no `name` or `repository`, so fall back to the
	// registry key and URL.
	const package_json: PackageJson = {
		name: cargo?.name ?? repo_name,
		version: cargo?.version ?? '0.0.0',
		repository: cargo?.repository ?? repo_url,
		private: true,
		...(cargo?.description ? { description: cargo.description } : null)
	};

	const library = new Library(library_json_from_modules(package_json, []));

	return {
		kind: 'cargo',
		library,
		package_json,
		repo_dir,
		entry
	};
};

export const local_repos_load = async ({
	local_repo_paths,
	log,
	git_ops = default_git_operations,
	npm_ops = default_npm_operations,
	parallel = true,
	concurrency = GITOPS_CONCURRENCY_DEFAULT,
	sync = true,
	allow_dirty = false
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
				await local_repo_load({ local_repo_path, log, git_ops, npm_ops, sync, allow_dirty })
			);
		}
		return loaded;
	}

	// Parallel loading with concurrency limit
	const results = await map_concurrent_settled(
		local_repo_paths,
		concurrency,
		async (local_repo_path) => {
			return local_repo_load({ local_repo_path, log, git_ops, npm_ops, sync, allow_dirty });
		}
	);

	// Check for failures and collect successes
	const loaded: Array<LocalRepo> = [];
	const errors: Array<{ repo_name: string; error: string }> = [];

	for (let i = 0; i < results.length; i++) {
		const result = results[i]!;
		if (result.status === 'fulfilled') {
			loaded.push(result.value);
		} else {
			const repo_path = local_repo_paths[i]!;
			errors.push({
				repo_name: repo_path.repo_name,
				error: String(result.reason)
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
