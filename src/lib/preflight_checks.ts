import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { styleText as st } from 'node:util';

import type { LocalRepo } from './local_repo.ts';
import type { NpmOperations, BuildOperations, ChangesetOperations } from './operations.ts';
import {
	default_npm_operations,
	default_build_operations,
	default_changeset_operations
} from './operations_defaults.ts';

export interface PreflightOptions {
	skip_changesets?: boolean;
	skip_build_validation?: boolean; // Skip build validation (useful for tests)
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
	npm_ops?: NpmOperations;
	build_ops?: BuildOperations;
	changeset_ops?: ChangesetOperations;
}

/**
 * Validates the publish-time requirements beyond repo git state:
 * - Changesets present (unless `skip_changesets`=true)
 * - Builds successful (fail-fast to prevent broken state)
 * - NPM authentication with username
 * - NPM registry connectivity
 *
 * Git state — each repo on its registry branch, clean, idle, in sync with
 * origin or ahead of it, and no live session in its checkout — is the readiness gate's,
 * which `gitops_publish --wetrun` runs before the plan's confirmation prompt
 * (`repo_readiness.ts`), so preflight reads no git.
 *
 * Build validation runs BEFORE any publishing to prevent the scenario where
 * version is bumped but build fails, leaving repo in broken state.
 *
 * @returns result with `ok`=false if any errors, plus warnings and detailed status
 */
export const run_preflight_checks = async ({
	repos,
	preflight_options = {},
	npm_ops = default_npm_operations,
	build_ops = default_build_operations,
	changeset_ops = default_changeset_operations
}: RunPreflightChecksOptions): Promise<PreflightResult> => {
	const {
		skip_changesets = false,
		skip_build_validation = false,
		estimate_time = true,
		log
	} = preflight_options;

	const warnings: Array<string> = [];
	const errors: Array<string> = [];
	const repos_with_changesets: Set<string> = new Set();
	const repos_without_changesets: Set<string> = new Set();
	let npm_username: string | undefined;
	let estimated_duration: number | undefined;

	log?.info(st('cyan', '✅ Running preflight checks...'));

	// 1. Check changesets (unless skipped)
	if (!skip_changesets) {
		log?.info('  Checking for changesets...');
		for (const repo of repos) {
			const has_result = await changeset_ops.has_changesets({ repo });
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

	// 2. Validate builds for packages with changesets
	if (!skip_build_validation && repos_with_changesets.size > 0) {
		log?.info(st('cyan', `  Validating builds for ${repos_with_changesets.size} package(s)...`));
		const repos_to_build = repos.filter((repo) => repos_with_changesets.has(repo.library.name));

		for (let i = 0; i < repos_to_build.length; i++) {
			const repo = repos_to_build[i]!;
			log?.info(
				st('dim', `    [${i + 1}/${repos_to_build.length}] Building ${repo.library.name}...`)
			);
			const build_result = await build_ops.build_package({ repo, log });
			if (!build_result.ok) {
				errors.push(
					`${repo.library.name} failed to build: ${build_result.output || build_result.message || 'unknown error'}`
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

	// 3. Check npm authentication with username
	log?.info('  Checking npm authentication...');
	const npm_auth_result = await npm_ops.check_auth();
	if (!npm_auth_result.ok) {
		errors.push(`npm authentication failed: ${npm_auth_result.message || 'not logged in'}`);
	} else {
		npm_username = npm_auth_result.username;
		log?.info(st('dim', `    Logged in as: ${npm_username}`));
	}

	// 4. Check network connectivity (npm registry)
	log?.info('  Checking npm registry connectivity...');
	const registry_result = await npm_ops.check_registry();
	if (!registry_result.ok) {
		warnings.push(`npm registry check failed: ${registry_result.message}`);
	}

	// 5. Estimate total publish time
	if (estimate_time) {
		const packages_to_publish = repos_with_changesets.size;
		if (packages_to_publish > 0) {
			// Rough estimate: 30s per package + 10s per package for NPM propagation
			estimated_duration = packages_to_publish * 40;
			log?.info(
				st(
					'dim',
					`  Estimated publish time: ~${Math.ceil(estimated_duration / 60)} minutes for ${packages_to_publish} package(s)`
				)
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
		npm_username
	};
};
