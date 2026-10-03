import { TaskError, type Task } from '@fuzdev/gro';
import { z } from 'zod';
import { readFile, writeFile } from 'node:fs/promises';
import { format_file } from '@fuzdev/gro/format_file.ts';
import { basename, join, resolve } from 'node:path';
import { print_path } from '@fuzdev/gro/paths.ts';
import { load_from_env } from '@fuzdev/gro/env.ts';
import { package_json_load } from '@fuzdev/gro/package_json.ts';
import { existsSync, readFileSync } from 'node:fs';
import { styleText as st } from 'node:util';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { compactReplacer } from 'svelte-docinfo';

import { fetch_repo_data } from './fetch_repo_data.ts';
import type { RepoJson } from './repo.svelte.ts';
import { create_fs_fetch_value_cache } from './fs_fetch_value_cache.ts';
import { resolve_gitops_repos } from './gitops_task_helpers.ts';
import { GITOPS_CONFIG_PATH_DEFAULT } from './gitops_constants.ts';
import {
	local_repos_load,
	local_repos_resolve,
	type LocalRepo,
	type LocalRepoPath
} from './local_repo.ts';
import { load_repos_status, to_repos_command } from './repos_status_load.ts';
import { check_gen_readiness } from './repo_readiness.ts';
import type { ReposOperations } from './operations.ts';
import { default_repos_operations } from './operations_defaults.ts';

// TODO add flag to ignore or invalidate cache -- no-cache? clean?

/** @nodocs */
export const Args = z.strictObject({
	config: z
		.string()
		.meta({ description: 'path to the gitops config file, absolute or relative to the cwd' })
		.default(GITOPS_CONFIG_PATH_DEFAULT),
	registry: z
		.string()
		.meta({
			description:
				'path to the repos.toml registry, when `repos` would not find it walking up from the cwd'
		})
		.optional(),
	outdir: z
		.string()
		.meta({ description: 'path to the directory for the generated files, defaults to $routes/' })
		.optional(),
	check: z
		.boolean()
		.meta({
			description:
				'report whether the repos are ready to generate from, as of the local refs, and exit non-zero if a real run would refuse; fetches nothing, needs no token, and writes nothing'
		})
		.default(false),
	allow_dirty: z
		.boolean()
		.meta({
			description:
				'read repos off their registry branch, dirty, or mid-operation as they sit, warning instead of refusing'
		})
		.default(false)
});
export type Args = z.infer<typeof Args>;

/**
 * Generates the dashboard's site data (`repos.json` and `repos.ts`) from each
 * configured repo's working tree and its GitHub CI status and pull requests.
 * It never moves a repo: see `prepare_gitops_sync` for what it reads and
 * refuses. This is a task not a `.gen.` file because it makes network calls.
 *
 * @nodocs
 */
export const task: Task<Args> = {
	Args,
	summary: 'generates UI data from repo metadata, reading each repo as it sits',
	run: async (ctx) => {
		const { args, log, invoke_task } = ctx;
		// `ctx.svelte_config` is a lazy getter, so it's only read when `outdir` isn't provided
		const {
			config,
			registry,
			outdir = (await ctx.svelte_config).routes_path,
			check,
			allow_dirty
		} = args;

		// The generated `repos.json` is the host project's site data, so a public host
		// must never carry a private repo's metadata: `host` refuses the private repos
		// the registry declares, before any fetch.
		const package_json = await package_json_load();

		const prepared = await prepare_gitops_sync({
			config,
			registry,
			host: { name: package_json.name, private: package_json.private === true },
			check,
			allow_dirty,
			log
		});
		if (!prepared) return;
		const { local_repos, token } = prepared;

		const outfile_json = resolve(outdir, 'repos.json');
		const outfile_ts = resolve(outdir, 'repos.ts');

		const cache = await create_fs_fetch_value_cache('repos');

		log.info('fetching remote repo data');
		const repos_json = await fetch_repo_data({ local_repos, token, cache: cache.data, log });

		// TODO should package_json be provided in the Gro task/gen contexts? check if it's always loaded
		const repo_specifier =
			package_json.name === '@fuzdev/fuz_repos'
				? '$lib/repo.svelte.ts'
				: '@fuzdev/fuz_repos/repo.svelte.ts';

		log.info(`generating ${outfile_json} and ${outfile_ts}`);

		// Generate repos.json with the raw data
		const json_contents = format_file(serialize_repos_json(repos_json), {
			lang: 'json'
		});
		const existing_json = existsSync(outfile_json) ? await readFile(outfile_json, 'utf8') : '';
		const json_changed = existing_json !== json_contents;
		if (json_changed) {
			log.info(`writing changes to ${print_path(outfile_json)}`);
			await writeFile(outfile_json, json_contents);
		} else {
			log.info(`no changes to ${print_path(outfile_json)}`);
		}

		// Generate repos.ts that imports from repos.json
		// TODO the `basename` is used here because we don't have an `origin_id` like with gen,
		// and this file gets re-exported,
		// and we don't want the file to change based on where it's being generated,
		// because for example linking to a local package would change the contents
		const ts_contents = `
			// generated by ${basename(import.meta.filename)} - do not edit

			import type {RepoJson} from '${repo_specifier}';

			import json from './repos.json' with {type: 'json'};

			export const repos_json: Array<RepoJson> = json as unknown as Array<RepoJson>;
		`;
		// TODO think about possibly using the `gen` functionality in this task, not sure what the API design could look like
		const formatted_ts = format_file(ts_contents, { filepath: outfile_ts });
		const existing_ts = existsSync(outfile_ts) ? await readFile(outfile_ts, 'utf8') : '';
		if (existing_ts === formatted_ts) {
			log.info(`no changes to ${print_path(outfile_ts)}`);
		} else {
			log.info(`writing changes to ${print_path(outfile_ts)}`);
			await writeFile(outfile_ts, formatted_ts);
		}

		if (json_changed) {
			await invoke_task('gen');
		}

		const changed = await cache.save();
		if (changed) {
			log.info('repos cache updated');
		} else {
			log.info('repos cache did not change');
		}
	}
};

/**
 * Serializes the site data for `repos.json`. Only each repo's `library_json`
 * is compacted with svelte-docinfo's `compactReplacer`, the format its schema
 * reads back: the replacer drops every `false` and empty array, which outside
 * the library data would strip a pull request's `draft: false`, an empty
 * `pull_requests`, and `package.json` fields like `private: false`. The
 * library data round-trips on its own rather than in one replacer pass:
 * `pkg_json` shares its nested objects (`exports`, `repository`) with the full
 * `package_json`, so a replacer keyed on object identity would compact those
 * there too.
 *
 * @param repos_json - the repos' data, in order
 * @returns unformatted JSON
 * @nodocs
 */
export const serialize_repos_json = (repos_json: Array<RepoJson>): string =>
	JSON.stringify(
		repos_json.map((repo_json) => ({
			...repo_json,
			library_json: JSON.parse(JSON.stringify(repo_json.library_json, compactReplacer))
		}))
	);

/**
 * The side effects `prepare_gitops_sync` reaches through, injectable for tests.
 *
 * @nodocs
 */
export interface GitopsSyncDeps {
	/** Runs `repos status`. */
	repos_ops: ReposOperations;
	/** Loads each resolved repo's library as it sits (`local_repos_load`). */
	load_repos: (options: {
		local_repo_paths: Array<LocalRepoPath>;
		log?: Logger;
	}) => Promise<Array<LocalRepo>>;
	/** Reads `SECRET_GITHUB_API_TOKEN`; `undefined` when it isn't set. */
	load_token: () => string | undefined;
	/** Whether a path exists, for the analysis-setup warning. */
	exists: (path: string) => boolean;
	/** Reads a file as UTF-8, for the analysis-setup warning. */
	read_file: (path: string) => string;
}

const default_gitops_sync_deps: GitopsSyncDeps = {
	repos_ops: default_repos_operations,
	load_repos: local_repos_load,
	// this searches the parent directory for the env var, so we don't use SvelteKit's $env imports
	load_token: () => load_from_env('SECRET_GITHUB_API_TOKEN') || undefined,
	exists: existsSync,
	read_file: (path) => readFileSync(path, 'utf8')
};

/**
 * Everything `gitops_sync` does before it fetches from GitHub, in order:
 *
 * 1. resolves the config's repos from `repos status <keys…> --json` (local
 *    refs), refusing a private repo when the host package is public
 * 2. checks each repo is ready to generate from (`check_gen_readiness`): on
 *    its registry branch, clean, and idle, unless `allow_dirty` — refusing
 *    before any network. With `check`, that report is the whole run: its
 *    warnings are logged, and it throws when a real run would refuse
 * 3. reads `SECRET_GITHUB_API_TOKEN`
 * 4. fetches the repos from origin (`repos status <keys…> --fetch --json`,
 *    which writes remote-tracking refs and nothing else) and checks them
 *    again, warning on a followed branch not in sync with origin and a
 *    failed fetch
 * 5. warns on each npm repo the library analysis can't fully read: no
 *    `node_modules`, or no `.svelte-kit/tsconfig.json` when its tsconfig
 *    extends it (external types then read as `any`) — it installs nothing
 * 6. loads each repo's library as it sits
 *
 * @returns the loaded repos and the token, or `null` when `check` passed
 * @throws {TaskError} if resolving fails, a repo isn't ready, or the token is missing
 * @nodocs
 */
export const prepare_gitops_sync = async (
	options: {
		config: string;
		registry?: string;
		host?: { name: string; private: boolean };
		check?: boolean;
		allow_dirty?: boolean;
		log: Logger;
	},
	deps: Partial<GitopsSyncDeps> = {}
): Promise<{ local_repos: Array<LocalRepo>; token: string } | null> => {
	const { config, registry, host, check = false, allow_dirty = false, log } = options;
	const { repos_ops, load_repos, load_token, exists, read_file } = {
		...default_gitops_sync_deps,
		...deps
	};
	const repos_command = to_repos_command(registry);

	const { config_path, gitops_config, report } = await resolve_gitops_repos({
		config,
		registry,
		host,
		log,
		repos_ops
	});
	const keys = gitops_config.repos;

	const local = check_gen_readiness({ report, keys, allow_dirty, repos_command });
	if (check) {
		log_gen_warnings(local.warnings, log);
		if (!local.ok) throw new TaskError(local.message);
		log.info(st('green', `all ${keys.length} repos are ready to generate the site data from`));
		return null;
	}
	if (!local.ok) throw new TaskError(local.message);

	const token = load_token();
	if (!token) {
		throw new TaskError('the env var SECRET_GITHUB_API_TOKEN was not found');
	}

	log.info(`fetching ${keys.length} repos from origin`);
	const fetched = await load_repos_status({ keys, registry, fetch: true, repos_ops });
	if (!fetched.ok) {
		throw new TaskError(`fetching the repos failed: ${fetched.message}`);
	}
	const resolved = local_repos_resolve({ keys, report: fetched.report, host, registry });
	if (!resolved.ok) {
		throw new TaskError(`${config_path}: ${resolved.message}`);
	}
	const checked = check_gen_readiness({
		report: fetched.report,
		keys,
		allow_dirty,
		repos_command
	});
	log_gen_warnings(checked.warnings, log);
	if (!checked.ok) throw new TaskError(checked.message);

	const local_repo_paths = resolved.value;
	const gaps = local_repo_paths
		.map((p) => ({ key: p.repo_name, missing: analysis_setup_gaps(p.repo_dir, exists, read_file) }))
		.filter((g) => g.missing.length > 0);
	if (gaps.length > 0) {
		log.warn(
			st(
				'yellow',
				"the library analysis reads these repos without their dependencies' types, so external types read as `any` — `npm install` and `gro sync` in each fixes it:"
			)
		);
		for (const { key, missing } of gaps) {
			log.warn(st('yellow', `  ${key}: no ${missing.join(' or ')}`));
		}
	}

	const local_repos = await load_repos({ local_repo_paths, log });
	return { local_repos, token };
};

const log_gen_warnings = (warnings: Array<string>, log: Logger): void => {
	if (warnings.length === 0) return;
	log.warn(st('yellow', "read as they sit, beside origin's CI:"));
	for (const line of warnings) log.warn(st('yellow', `  ${line}`));
};

/**
 * What an npm repo lacks for the library analysis to resolve its dependencies'
 * types: `node_modules`, and `.svelte-kit/tsconfig.json` when its
 * `tsconfig.json` extends it. Empty for a repo without a `package.json`.
 */
const analysis_setup_gaps = (
	repo_dir: string,
	exists: (path: string) => boolean,
	read_file: (path: string) => string
): Array<string> => {
	if (!exists(join(repo_dir, 'package.json'))) return [];
	const missing: Array<string> = [];
	if (!exists(join(repo_dir, 'node_modules'))) missing.push('`node_modules`');
	const tsconfig_path = join(repo_dir, 'tsconfig.json');
	const svelte_kit_tsconfig = '.svelte-kit/tsconfig.json';
	if (
		exists(tsconfig_path) &&
		read_file(tsconfig_path).includes(svelte_kit_tsconfig) &&
		!exists(join(repo_dir, svelte_kit_tsconfig))
	) {
		missing.push(`\`${svelte_kit_tsconfig}\``);
	}
	return missing;
};
