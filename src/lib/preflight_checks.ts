import type {Logger} from '@fuzdev/fuz_util/log.js';
import type {Result} from '@fuzdev/fuz_util/result.js';
import {spawn_out} from '@fuzdev/fuz_util/process.js';
import {styleText as st} from 'node:util';

import type {LocalRepo} from './local_repo.js';
import type {
	GitOperations,
	NpmOperations,
	BuildOperations,
	ChangesetOperations,
} from './operations.js';
import {
	default_git_operations,
	default_npm_operations,
	default_build_operations,
	default_changeset_operations,
} from './operations_defaults.js';

export interface PreflightOptions {
	skip_changesets?: boolean;
	skip_build_validation?: boolean; // Skip build validation (useful for tests)
	required_branch?: string;
	check_remote?: boolean; // Check if git remote is reachable
	estimate_time?: boolean; // Estimate total publish time
	log?: Logger;
}

export interface PreflightResult {
	ok: boolean;
	warnings: Array<string>;
	errors: Array<string>;
	repos_with_changesets: Set<string>;
	repos_without_changesets: Set<string>;
	estimated_duration?: number; // In seconds
	npm_username?: string;
}

export interface RunPreflightChecksOptions {
	repos: Array<LocalRepo>;
	preflight_options?: PreflightOptions;
	git_ops?: GitOperations;
	npm_ops?: NpmOperations;
	build_ops?: BuildOperations;
	changeset_ops?: ChangesetOperations;
}

/**
 * Validates all requirements before publishing can proceed.
 *
 * Performs comprehensive pre-flight validation:
 * - Clean workspaces (100% clean required - no uncommitted changes)
 * - Correct branch (usually main)
 * - Changesets present (unless skip_changesets=true)
 * - Builds successful (fail-fast to prevent broken state)
 * - Git remote reachability
 * - NPM authentication with username
 * - NPM registry connectivity
 *
 * Build validation runs BEFORE any publishing to prevent the scenario where
 * version is bumped but build fails, leaving repo in broken state.
 *
 * @returns result with ok=false if any errors, plus warnings and detailed status
 */
export const run_preflight_checks = async ({
	repos,
	preflight_options = {},
	git_ops = default_git_operations,
	npm_ops = default_npm_operations,
	build_ops = default_build_operations,
	changeset_ops = default_changeset_operations,
}: RunPreflightChecksOptions): Promise<PreflightResult> => {
	const {
		skip_changesets = false,
		skip_build_validation = false,
		required_branch = 'main',
		check_remote = true,
		estimate_time = true,
		log,
	} = preflight_options;

	const warnings: Array<string> = [];
	const errors: Array<string> = [];
	const repos_with_changesets: Set<string> = new Set();
	const repos_without_changesets: Set<string> = new Set();
	let npm_username: string | undefined;
	let estimated_duration: number | undefined;

	log?.info(st('cyan', '✅ Running preflight checks...'));

	// 1. Check clean workspaces - must be 100% clean before publishing
	log?.info('  Checking workspace cleanliness...');
	for (const repo of repos) {
		const clean_result = await git_ops.check_clean_workspace({cwd: repo.repo_dir}); // eslint-disable-line no-await-in-loop
		if (!clean_result.ok) {
			errors.push(`${repo.library.name} failed workspace check: ${clean_result.message}`);
			continue;
		}

		if (!clean_result.value) {
			// Get list of changed files for better error message
			const files_result = await git_ops.get_changed_files({cwd: repo.repo_dir}); // eslint-disable-line no-await-in-loop
			if (files_result.ok) {
				// No filtering - workspace must be 100% clean
				const unexpected_files = files_result.value;

				if (unexpected_files.length > 0) {
					errors.push(
						`${repo.library.name} has uncommitted changes in: ${unexpected_files.slice(0, 3).join(', ')}${unexpected_files.length > 3 ? ` and ${unexpected_files.length - 3} more` : ''}`,
					);
				}
			} else {
				errors.push(`${repo.library.name} has uncommitted changes`);
			}
		}
	}

	// 2. Check correct branch
	log?.info(`  Checking branches (expecting ${required_branch})...`);
	for (const repo of repos) {
		const branch_result = await git_ops.current_branch_name({cwd: repo.repo_dir}); // eslint-disable-line no-await-in-loop
		if (!branch_result.ok) {
			errors.push(`${repo.library.name} failed branch check: ${branch_result.message}`);
			continue;
		}

		if (branch_result.value !== required_branch) {
			errors.push(
				`${repo.library.name} is on branch '${branch_result.value}', expected '${required_branch}'`,
			);
		}
	}

	// 3. Check changesets (unless skipped)
	if (!skip_changesets) {
		log?.info('  Checking for changesets...');
		for (const repo of repos) {
			const has_result = await changeset_ops.has_changesets({repo}); // eslint-disable-line no-await-in-loop
			if (!has_result.ok) {
				errors.push(`${repo.library.name} failed changeset check: ${has_result.message}`);
				continue;
			}

			if (has_result.value) {
				repos_with_changesets.add(repo.library.name);
			} else {
				repos_without_changesets.add(repo.library.name);
				warnings.push(`${repo.library.name} has no changesets`);
			}
		}

		if (repos_without_changesets.size > 0) {
			log?.warn(st('yellow', `  ⚠️  ${repos_without_changesets.size} packages have no changesets`));
		}
	}

	// 4. Validate builds for packages with changesets
	if (!skip_build_validation && repos_with_changesets.size > 0) {
		log?.info(st('cyan', `  Validating builds for ${repos_with_changesets.size} package(s)...`));
		const repos_to_build = repos.filter((repo) => repos_with_changesets.has(repo.library.name));

		for (let i = 0; i < repos_to_build.length; i++) {
			const repo = repos_to_build[i]!;
			log?.info(
				st('dim', `    [${i + 1}/${repos_to_build.length}] Building ${repo.library.name}...`),
			);
			const build_result = await build_ops.build_package({repo, log}); // eslint-disable-line no-await-in-loop
			if (!build_result.ok) {
				errors.push(
					`${repo.library.name} failed to build: ${build_result.output || build_result.message || 'unknown error'}`,
				);
			} else {
				log?.info(st('dim', `    ✓ ${repo.library.name} built successfully`));
			}
		}

		if (errors.some((err) => err.includes('failed to build'))) {
			log?.error(st('red', '  ❌ Build validation failed - fix build errors before publishing'));
		} else {
			log?.info(st('green', '  ✓ All builds validated successfully'));
		}
	}

	// 5. Check git remote reachability (skip in tests when check_remote is false)
	if (check_remote && repos.length > 0) {
		log?.info('  Checking git remote connectivity...');
		// Only check first repo to avoid slowing down tests with multiple remote checks
		const remote_result = await check_git_remote(repos[0]!.repo_dir);
		if (!remote_result.ok) {
			warnings.push(`git remote may not be reachable - ${remote_result.message}`);
		}
	}

	// 6. Check npm authentication with username
	log?.info('  Checking npm authentication...');
	const npm_auth_result = await npm_ops.check_auth();
	if (!npm_auth_result.ok) {
		errors.push(`npm authentication failed: ${npm_auth_result.message || 'not logged in'}`);
	} else {
		npm_username = npm_auth_result.username;
		log?.info(st('dim', `    Logged in as: ${npm_username}`));
	}

	// 7. Check network connectivity (npm registry)
	log?.info('  Checking npm registry connectivity...');
	const registry_result = await npm_ops.check_registry();
	if (!registry_result.ok) {
		warnings.push(`npm registry check failed: ${registry_result.message}`);
	}

	// 8. Estimate total publish time
	if (estimate_time) {
		const packages_to_publish = repos_with_changesets.size;
		if (packages_to_publish > 0) {
			// Rough estimate: 30s per package + 10s per package for NPM propagation
			estimated_duration = packages_to_publish * 40;
			log?.info(
				st(
					'dim',
					`  Estimated publish time: ~${Math.ceil(estimated_duration / 60)} minutes for ${packages_to_publish} package(s)`,
				),
			);
		}
	}

	// Report results
	const ok = errors.length === 0;

	if (errors.length > 0) {
		log?.error(st('red', `\n❌ Preflight checks failed with ${errors.length} errors:`));
		for (const error of errors) {
			log?.error(`  - ${error}`);
		}
	}

	if (warnings.length > 0) {
		log?.warn(st('yellow', `\n⚠️  Preflight checks found ${warnings.length} warnings:`));
		for (const warning of warnings) {
			log?.warn(`  - ${warning}`);
		}
	}

	if (ok) {
		log?.info(st('green', '\n✨ All preflight checks passed!'));
	}

	return {
		ok,
		warnings,
		errors,
		repos_with_changesets,
		repos_without_changesets,
		estimated_duration,
		npm_username,
	};
};

const check_git_remote = async (cwd: string): Promise<Result<object, {message: string}>> => {
	try {
		// Try to fetch refs from remote without downloading objects
		const result = await spawn_out('git', ['ls-remote', '--heads', 'origin'], {cwd});
		if (result.stdout || result.stderr) {
			return {ok: true};
		}
		return {ok: false, message: 'No response from git remote'};
	} catch (error) {
		return {ok: false, message: String(error)};
	}
};
