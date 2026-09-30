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

import { gitops_config_leaked_private_repos } from './gitops_config.ts';
import type { ReposEntryStatus, ReposStatusReport } from './repos_status.ts';
import { GITOPS_CONCURRENCY_DEFAULT } from './gitops_constants.ts';
import { cargo_toml_load } from './cargo_toml.ts';
import { to_repos_command } from './repos_status_load.ts';

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
	const repos_command = to_repos_command(registry);
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
 * Loads a resolved repo as its working tree sits (the tasks read where each
 * repo sits from `repos status`, and `repos sync` moves them). Moves no ref;
 * gro caches the library at `.gro/library.json` in the repo, at a clean commit.
 *
 * 1. Loads `library_json` via `library_load_from_repo` (svelte-docinfo analysis)
 * 2. Creates `Library` and extracts dependency maps
 *
 * A repo with no `package.json` but a Rust `Cargo.toml` loads as a
 * dashboard-only `cargo` repo instead.
 *
 * @throws {TaskError} if the analysis fails
 */
export const local_repo_load = async ({
	local_repo_path,
	log
}: {
	local_repo_path: LocalRepoPath;
	log?: Logger;
}): Promise<LocalRepo> => {
	const { entry, repo_dir, repo_name } = local_repo_path;

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
		({ library_json, package_json } = await library_load_from_repo(repo_dir, { log }));
	} catch (err) {
		throw new TaskError(
			`Failed to load library metadata for repo "${repo_name}" in ${repo_dir}: ${to_error_message(err)}`
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
 * dependency analysis. Non-npm repos (e.g. Rust `cargo` repos) are still
 * rendered on the dashboard, but excluded from the changeset cascade.
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
	parallel = true,
	concurrency = GITOPS_CONCURRENCY_DEFAULT
}: {
	local_repo_paths: Array<LocalRepoPath>;
	log?: Logger;
	parallel?: boolean;
	concurrency?: number;
}): Promise<Array<LocalRepo>> => {
	if (!parallel) {
		// sequential loading
		const loaded: Array<LocalRepo> = [];
		for (const local_repo_path of local_repo_paths) {
			loaded.push(await local_repo_load({ local_repo_path, log }));
		}
		return loaded;
	}

	// Parallel loading with concurrency limit
	const results = await map_concurrent_settled(
		local_repo_paths,
		concurrency,
		async (local_repo_path) => {
			return local_repo_load({ local_repo_path, log });
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
