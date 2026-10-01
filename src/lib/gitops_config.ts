/**
 * The gitops config: which repos a project's gitops tasks work over, as a flat
 * list of `repos.toml` registry keys.
 *
 * Every other fact about a repo — its dir, URL, branch, visibility, `ci`, and
 * `archived` — comes from the registry, through `repos status --json`, so the
 * config declares nothing that could drift from it.
 *
 * @module
 */

import { existsSync } from 'node:fs';
import { z } from 'zod';

import type { ReposEntryStatus } from './repos_status.ts';

/** A project's gitops config. */
export const GitopsConfig = z.strictObject({
	repos: z
		.array(z.string().min(1))
		.meta({ description: 'repos.toml registry keys of owned repos, in the order tasks list them' })
});
export type GitopsConfig = z.infer<typeof GitopsConfig>;

/** A config module's default export in function form. */
export type CreateGitopsConfig = () => GitopsConfig | Promise<GitopsConfig>;

export interface GitopsConfigModule {
	readonly default: GitopsConfig | CreateGitopsConfig;
}

/**
 * The private repos a public host package must not publish. `gitops_sync` writes
 * every configured repo's GitHub metadata into the host project's generated
 * `repos.json` — a public site's data when the host package is public — so a
 * private repo in that config would leak. Empty when the host is private.
 *
 * @param entries - the configured repos' registry entries
 * @param host_is_private - whether the host `package.json` sets `private: true`
 */
export const gitops_config_leaked_private_repos = (
	entries: ReadonlyArray<ReposEntryStatus>,
	host_is_private: boolean
): Array<ReposEntryStatus> =>
	host_is_private ? [] : entries.filter((e) => e.visibility === 'private');

/**
 * Loads a gitops config module and validates it.
 *
 * @returns the config, or `null` when no file exists at `config_path`
 * @throws {Error} if the module's default export isn't a valid config
 */
export const load_gitops_config = async (config_path: string): Promise<GitopsConfig | null> => {
	if (!existsSync(config_path)) {
		// No user config file found.
		return null;
	}
	// Import the user's `gitops.config.ts`. An import or call failure keeps its
	// stack, which carries the config's file and line; validation errors don't need one.
	let config_module: unknown;
	try {
		config_module = await import(config_path);
	} catch (err) {
		throw Error(`Failed to import the gitops config at ${config_path}:\n${error_with_stack(err)}`);
	}
	validate_gitops_config_module(config_module, config_path);
	let raw: unknown;
	try {
		raw =
			typeof config_module.default === 'function'
				? await config_module.default()
				: config_module.default;
	} catch (err) {
		throw Error(`The gitops config at ${config_path} threw:\n${error_with_stack(err)}`);
	}
	return parse_gitops_config(raw, config_path);
};

const error_with_stack = (err: unknown): string =>
	err instanceof Error ? (err.stack ?? err.message) : String(err);

/**
 * Validates a loaded config value: registry keys, each listed once.
 *
 * @throws {Error} naming the config and what's wrong with it
 */
export const parse_gitops_config = (raw: unknown, config_path: string): GitopsConfig => {
	const parsed = GitopsConfig.safeParse(raw);
	if (!parsed.success) {
		const hint =
			"\n  `repos` lists repos.toml registry keys; a repo's url, dir, branch, visibility, ci, and archived come from the registry";
		throw Error(
			`Invalid gitops config at ${config_path}:\n${z.prettifyError(parsed.error)}${hint}`
		);
	}
	const config = parsed.data;
	const problems: Array<string> = [];
	const seen: Set<string> = new Set();
	for (const key of config.repos) {
		if (key.includes('/')) {
			problems.push(`\`${key}\` isn't a registry key — list repos by their repos.toml key`);
		} else if (seen.has(key)) {
			problems.push(`\`${key}\` is listed more than once`);
		}
		seen.add(key);
	}
	if (problems.length) {
		throw Error(`Invalid gitops config at ${config_path}:\n  ${problems.join('\n  ')}`);
	}
	return config;
};

export const validate_gitops_config_module: (
	config_module: any,
	config_path: string
) => asserts config_module is GitopsConfigModule = (config_module, config_path) => {
	const config = config_module.default;
	if (!config) {
		throw Error(`Invalid gitops config module at ${config_path}: expected a default export`);
	} else if (!(typeof config === 'function' || typeof config === 'object')) {
		throw Error(
			`Invalid gitops config module at ${config_path}: the default export must be a function or object`
		);
	}
};
