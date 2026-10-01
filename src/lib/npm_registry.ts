import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { spawn_out } from '@fuzdev/fuz_util/process.ts';
import { wait } from '@fuzdev/fuz_util/async.ts';
import { styleText as st } from 'node:util';

export interface WaitOptions {
	log?: Logger;
	max_attempts?: number;
	initial_delay?: number;
	max_delay?: number;
	timeout?: number;
}

/**
 * The side effects of the registry checks — running npm, sleeping, and reading
 * the clock — injected so tests drive them with plain objects.
 */
export interface NpmRegistryDeps {
	/**
	 * Runs `npm` with `args` and resolves to what it wrote to stdout — empty when
	 * nothing, or when npm couldn't run. A throw counts as unavailable too.
	 */
	run_npm: (args: Array<string>) => Promise<string>;
	/** Sleeps for `ms` milliseconds. */
	wait: (ms: number) => Promise<void>;
	/** The current time in milliseconds, as `Date.now`. */
	now: () => number;
}

export const default_npm_registry_deps: NpmRegistryDeps = {
	run_npm: async (args) => (await spawn_out('npm', args)).stdout ?? '',
	wait,
	now: () => Date.now()
};

/**
 * Checks whether `pkg@version` is on the npm registry, by `npm view`.
 *
 * @returns `true` when the registry reports that exact version, `false` when it
 * doesn't or npm couldn't run
 */
export const check_package_available = async (
	pkg: string,
	version: string,
	options: { log?: Logger } = {},
	deps: NpmRegistryDeps = default_npm_registry_deps
): Promise<boolean> => {
	const { log } = options;
	try {
		const stdout = await deps.run_npm(['view', `${pkg}@${version}`, 'version']);
		return stdout.trim() === version;
	} catch (error) {
		log?.debug(`Failed to check ${pkg}@${version}: ${error}`);
		return false;
	}
};

/**
 * Waits for package version to propagate to NPM registry.
 *
 * Uses exponential backoff with jitter to avoid hammering registry.
 * Logs progress every 5 attempts. Respects timeout to avoid infinite waits.
 *
 * Critical for multi-repo publishing: ensures published packages are available
 * before updating dependent packages.
 *
 * @param options.max_attempts - max poll attempts (default 30)
 * @param options.initial_delay - starting delay in ms (default 1000)
 * @param options.max_delay - max delay between attempts (default 60000)
 * @param options.timeout - total timeout in ms (default 300000 = 5min)
 * @throws {Error} if timeout reached or max attempts exceeded
 */
export const wait_for_package = async (
	pkg: string,
	version: string,
	options: WaitOptions = {},
	deps: NpmRegistryDeps = default_npm_registry_deps
): Promise<void> => {
	const {
		log,
		max_attempts = 30,
		initial_delay = 1000,
		max_delay = 60000,
		timeout = 300000 // 5 minutes default
	} = options;

	const start_time = deps.now();
	let attempt = 0;
	let delay = initial_delay;

	while (attempt < max_attempts) {
		attempt++;

		if (deps.now() - start_time > timeout) {
			throw new Error(`Timeout waiting for ${pkg}@${version} after ${timeout}ms`);
		}

		if (await check_package_available(pkg, version, { log }, deps)) {
			log?.info(st('green', `    ✓ ${pkg}@${version} is now available on NPM`));
			return;
		}

		// Log progress occasionally
		if (attempt % 5 === 0) {
			log?.info(st('dim', `    Still waiting... (attempt ${attempt}/${max_attempts})`));
		}

		// Wait with exponential backoff + jitter
		const jitter = Math.random() * delay * 0.1; // 10% jitter
		const actual_delay = Math.min(delay + jitter, max_delay);
		await deps.wait(actual_delay);

		// Exponential backoff
		delay = Math.min(delay * 1.5, max_delay);
	}

	throw new Error(`${pkg}@${version} not available after ${max_attempts} attempts`);
};
