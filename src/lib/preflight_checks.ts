import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { styleText as st } from 'node:util';

import type { LocalRepo } from './local_repo.ts';
import type { VersionChange } from './publishing_plan.ts';
import type { NpmOperations, BuildOperations } from './operations.ts';
import { default_npm_operations, default_build_operations } from './operations_defaults.ts';

export interface PreflightResult {
	ok: boolean;
	warnings: Array<string>;
	errors: Array<string>;
}

export interface RunPreflightChecksOptions {
	repos: Array<LocalRepo>;
	/**
	 * The plan's version changes — every package the run publishes, explicit,
	 * escalated, and auto-generated alike. Preflight builds exactly these.
	 */
	version_changes: Array<VersionChange>;
	log?: Logger;
	npm_ops?: NpmOperations;
	build_ops?: BuildOperations;
}

/**
 * Validates the publish-time requirements beyond repo git state:
 * - every package the plan publishes builds (fail-fast to prevent broken state)
 * - npm authentication
 * - npm registry connectivity
 *
 * What publishes is the plan's to decide, so preflight reads no changesets: it
 * builds each package in `version_changes`. Git state — each repo on its
 * registry branch, clean, idle, in sync with origin or ahead of it, and no live
 * session in its checkout — is the readiness gate's, which `gitops_publish
 * --wetrun` runs before the plan's confirmation prompt (`repo_readiness.ts`), so
 * preflight reads no git.
 *
 * Build validation runs BEFORE any publishing to prevent the scenario where
 * version is bumped but build fails, leaving repo in broken state.
 *
 * @returns result with `ok`=false if any errors, plus warnings
 */
export const run_preflight_checks = async ({
	repos,
	version_changes,
	log,
	npm_ops = default_npm_operations,
	build_ops = default_build_operations
}: RunPreflightChecksOptions): Promise<PreflightResult> => {
	const warnings: Array<string> = [];
	const errors: Array<string> = [];

	log?.info(st('cyan', '✅ Running preflight checks...'));

	// 1. Build every package the plan publishes
	const repo_by_name: Map<string, LocalRepo> = new Map(repos.map((r) => [r.library.name, r]));
	const repos_to_build: Array<LocalRepo> = [];
	for (const change of version_changes) {
		const repo = repo_by_name.get(change.package_name);
		if (repo) {
			repos_to_build.push(repo);
		} else {
			errors.push(`${change.package_name} is in the plan but not among the repos`);
		}
	}

	if (repos_to_build.length > 0) {
		log?.info(st('cyan', `  Validating builds for ${repos_to_build.length} package(s)...`));
		let build_failures = 0;
		for (let i = 0; i < repos_to_build.length; i++) {
			const repo = repos_to_build[i]!;
			log?.info(
				st('dim', `    [${i + 1}/${repos_to_build.length}] Building ${repo.library.name}...`)
			);
			const build_result = await build_ops.build_package({ repo });
			if (build_result.ok) {
				log?.info(st('dim', `    ✓ ${repo.library.name} built successfully`));
			} else {
				build_failures++;
				errors.push(
					`${repo.library.name} failed to build: ${build_result.output || build_result.message || 'unknown error'}`
				);
			}
		}

		if (build_failures > 0) {
			log?.error(st('red', '  ❌ Build validation failed - fix build errors before publishing'));
		} else {
			log?.info(st('green', '  ✓ All builds validated successfully'));
		}
	}

	// 2. Check npm authentication
	log?.info('  Checking npm authentication...');
	const npm_auth_result = await npm_ops.check_auth();
	if (npm_auth_result.ok) {
		log?.info(st('dim', `    Logged in as: ${npm_auth_result.username}`));
	} else {
		errors.push(`npm authentication failed: ${npm_auth_result.message || 'not logged in'}`);
	}

	// 3. Check network connectivity (npm registry)
	log?.info('  Checking npm registry connectivity...');
	const registry_result = await npm_ops.check_registry();
	if (!registry_result.ok) {
		warnings.push(`npm registry check failed: ${registry_result.message}`);
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

	return { ok, warnings, errors };
};
