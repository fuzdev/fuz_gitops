/**
 * Shared initialization logic for all gitops tasks.
 *
 * `resolve_gitops_repos()` loads the config's registry keys, runs
 * `repos status <keys…> --json`, and resolves each key to its checkout.
 * `get_gitops_ready()` then loads each repo's library as its working tree
 * sits. `gate_publish_readiness()` is the read-only gate a real publish runs
 * before its prompt, and `log_readiness_block()` the diagnostics' report of
 * repos not at rest (both over `repo_readiness.ts`).
 *
 * Used by: `gitops_sync.task.ts`, `gitops_analyze.task.ts`, `gitops_plan.task.ts`,
 * `gitops_publish.task.ts`, `gitops_validate.task.ts`, and `gitops_run.task.ts`.
 *
 * Accepts `repos_ops` to support testing via the operations pattern (see
 * `operations.ts` for dependency injection details).
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
	repo_is_npm,
	type LocalRepo,
	type LocalRepoPath
} from './local_repo.ts';
import { load_repos_status } from './repos_status_load.ts';
import {
	check_publish_readiness,
	format_readiness_ahead,
	format_readiness_block,
	repos_not_at_rest
} from './repo_readiness.ts';
import type { ReposStatusReport } from './repos_status.ts';
import type { ReposOperations } from './operations.ts';
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
	parallel?: boolean;
	concurrency?: number;
}

/**
 * Central initialization function for the gitops tasks that load libraries:
 * resolves the config's repos through `repos status` (`resolve_gitops_repos`),
 * then loads each repo's library as its working tree sits (`local_repos_load`).
 * Changes nothing.
 *
 * @param options.repos_ops - for testing (defaults to running the `repos` binary)
 * @param options.parallel - whether to load repos in parallel (default: true)
 * @param options.concurrency - max concurrent repo loads (default: 5)
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
	const { log, parallel, concurrency } = options;
	const { config_path, gitops_config, local_repo_paths } = await resolve_gitops_repos(options);
	const local_repos = await local_repos_load({ local_repo_paths, log, parallel, concurrency });
	return { config_path, gitops_config, local_repos };
};

export interface GatePublishReadinessOptions {
	/** The loaded repos; the npm ones are gated. */
	local_repos: ReadonlyArray<LocalRepo>;
	/** A `repos.toml` to use instead of the one `repos` finds walking up from the cwd. */
	registry?: string;
	/**
	 * The package names the plan publishes, to say whether a repo's commits ahead
	 * of origin go out with its release or stay unpushed.
	 */
	publishing?: ReadonlySet<string>;
	log?: Logger;
	repos_ops?: ReposOperations;
}

/**
 * The readiness gate `gitops_publish --wetrun` runs before its confirmation
 * prompt: fetches every npm repo from origin (`repos status <keys…> --fetch
 * --json`, which writes remote-tracking refs and nothing else) and refuses
 * unless each is ready (`check_publish_readiness`) — on its registry branch,
 * clean, idle, in sync with origin or ahead of it, fetched without error, no
 * other live session in its checkout, and nothing left to a person. Each
 * ready repo ahead of origin is logged, saying whether its release push
 * carries those commits or they stay unpushed.
 *
 * Every npm repo, not just those the plan publishes or rewrites: the plan
 * reads each one's working tree (changesets, versions, dependency ranges), so
 * a repo off its branch or behind origin can hide a changeset and leave the
 * plan wrong about what to publish. Changes nothing.
 *
 * @param options - the loaded repos, the `--registry` if any, the package names the plan publishes, a logger, and the `repos` runner
 * @throws {TaskError} if `repos status` fails or any npm repo isn't ready, naming each problem and its fix
 */
export const gate_publish_readiness = async (
	options: GatePublishReadinessOptions
): Promise<void> => {
	const { local_repos, registry, publishing, log, repos_ops = default_repos_operations } = options;
	const npm_repos = local_repos.filter(repo_is_npm);
	const keys = npm_repos.map((r) => r.entry.key);
	if (keys.length === 0) return;

	log?.info(`fetching ${keys.length} npm repos to check they're ready to publish`);
	const loaded = await load_repos_status({ keys, registry, fetch: true, repos_ops });
	if (!loaded.ok) {
		throw new TaskError(`the readiness check failed: ${loaded.message}`);
	}
	const checked = check_publish_readiness({
		report: loaded.report,
		keys,
		repos_command: registry === undefined ? 'repos' : `repos --registry ${registry}`
	});
	if (!checked.ok) {
		throw new TaskError(checked.message);
	}
	log?.info(st('green', `all ${keys.length} npm repos are ready to publish`));
	const name_by_key = new Map(npm_repos.map((r) => [r.entry.key, r.library.name] as const));
	for (const ahead of checked.ahead) {
		const name = name_by_key.get(ahead.key);
		const publishes = name !== undefined && publishing?.has(name) === true;
		log?.info(st('yellow', format_readiness_ahead(ahead, publishes)));
	}
};

/**
 * Logs the diagnostics' readiness block as warnings (stderr, so a `--format
 * json` or `markdown` document on stdout stays clean): each npm repo not at
 * rest, and how. Logs nothing when every one is at rest. Reads the entries
 * `repos status` already reported, from local refs.
 *
 * @param local_repos - the loaded repos; the npm ones are reported
 * @param log - where the warnings go
 * @param now - the current time in unix seconds (defaults to the clock)
 */
export const log_readiness_block = (
	local_repos: ReadonlyArray<LocalRepo>,
	log: Logger,
	now: number = Math.floor(Date.now() / 1000)
): void => {
	const not_ready = repos_not_at_rest(local_repos.filter(repo_is_npm).map((r) => r.entry));
	for (const line of format_readiness_block(not_ready, now)) {
		log.warn(st('yellow', line));
	}
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
