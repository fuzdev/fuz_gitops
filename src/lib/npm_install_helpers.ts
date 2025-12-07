import type {Logger} from '@fuzdev/fuz_util/log.js';
import {styleText as st} from 'node:util';

import type {LocalRepo} from './local_repo.js';
import type {GitopsOperations} from './operations.js';

/**
 * Checks if an npm install error is caused by stale cache (ETARGET).
 * Detects various error message formats:
 * - "code ETARGET"
 * - "ETARGET"
 * - "notarget"
 * - "No matching version found"
 */
const is_etarget_error = (message: string, stderr: string): boolean => {
	const combined = `${message} ${stderr}`.toLowerCase();
	return (
		combined.includes('etarget') ||
		combined.includes('notarget') ||
		combined.includes('no matching version found')
	);
};

/**
 * Installs npm dependencies with cache healing on ETARGET errors.
 *
 * **Strategy:**
 * 1. First attempt: regular `npm install`
 * 2. On ETARGET error (stale cache): `npm cache clean --force` then retry
 * 3. On other errors: fail immediately
 *
 * **Why ETARGET errors occur:**
 * After publishing a package and waiting for NPM registry propagation,
 * npm's local cache may still have stale "404" metadata. This healing
 * strategy clears the cache to force fresh metadata fetch.
 *
 * @param repo - The repository to install dependencies for
 * @param ops - Gitops operations (for dependency injection)
 * @param log - Optional logger
 * @throws Error if install fails (with details about cache healing attempts)
 */
export const install_with_cache_healing = async (
	repo: LocalRepo,
	ops: GitopsOperations,
	log?: Logger,
): Promise<void> => {
	// First attempt
	const install_result = await ops.npm.install({cwd: repo.repo_dir});

	if (install_result.ok) {
		return; // Success
	}

	// Check if error is ETARGET (package not found due to stale cache)
	const stderr = install_result.stderr || '';
	const message = install_result.message || '';

	if (!is_etarget_error(message, stderr)) {
		// Different error - fail immediately without cache healing
		throw new Error(
			`Failed to install dependencies in ${repo.library.name}: ${install_result.message}${stderr ? `\n${stderr}` : ''}`,
		);
	}

	// ETARGET error - try cache healing
	log?.warn(st('yellow', `  ⚠️  ETARGET error detected - cleaning npm cache...`));

	const cache_result = await ops.npm.cache_clean();
	if (!cache_result.ok) {
		throw new Error(`Failed to clean npm cache: ${cache_result.message}`);
	}

	log?.info('  ✓ Cache cleaned, retrying install...');

	// Retry install after cache clean
	const retry_result = await ops.npm.install({cwd: repo.repo_dir});

	if (!retry_result.ok) {
		throw new Error(
			`Failed to install dependencies after cache clean in ${repo.library.name}: ${retry_result.message}${retry_result.stderr ? `\n${retry_result.stderr}` : ''}`,
		);
	}

	log?.info(st('green', `  ✓ Dependencies installed successfully after cache heal`));
};
