import type {Logger} from '@fuzdev/fuz_util/log.js';
import {spawn_out} from '@fuzdev/fuz_util/process.js';
import {wait} from '@fuzdev/fuz_util/async.js';
import {styleText as st} from 'node:util';

export interface WaitOptions {
	log?: Logger;
	max_attempts?: number;
	initial_delay?: number;
	max_delay?: number;
	timeout?: number;
}

export interface PackageInfo {
	name: string;
	version: string;
}

export const check_package_available = async (
	pkg: string,
	version: string,
	options: {log?: Logger} = {},
): Promise<boolean> => {
	const {log} = options;
	try {
		// Use npm view to check if the specific version exists
		const result = await spawn_out('npm', ['view', `${pkg}@${version}`, 'version']);

		if (result.stdout) {
			const output = result.stdout.trim();
			// If we get a version back, it exists
			return output === version;
		}

		return false;
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
 * @param options.max_attempts max poll attempts (default 30)
 * @param options.initial_delay starting delay in ms (default 1000)
 * @param options.max_delay max delay between attempts (default 60000)
 * @param options.timeout total timeout in ms (default 300000 = 5min)
 * @throws {Error} if timeout reached or max attempts exceeded
 */
export const wait_for_package = async (
	pkg: string,
	version: string,
	options: WaitOptions = {},
): Promise<void> => {
	const {
		log,
		max_attempts = 30,
		initial_delay = 1000,
		max_delay = 60000,
		timeout = 300000, // 5 minutes default
	} = options;

	const start_time = Date.now();
	let attempt = 0;
	let delay = initial_delay;

	while (attempt < max_attempts) {
		attempt++;

		// Check timeout
		if (Date.now() - start_time > timeout) {
			throw new Error(`Timeout waiting for ${pkg}@${version} after ${timeout}ms`);
		}

		// Check if package is available
		// eslint-disable-next-line no-await-in-loop
		if (await check_package_available(pkg, version, {log})) {
			log?.info(st('green', `    ✓ ${pkg}@${version} is now available on NPM`));
			return;
		}

		// Log progress occasionally
		if (attempt > 0 && attempt % 5 === 0) {
			log?.info(st('dim', `    Still waiting... (attempt ${attempt}/${max_attempts})`));
		}

		// Wait with exponential backoff + jitter
		const jitter = Math.random() * delay * 0.1; // 10% jitter
		const actual_delay = Math.min(delay + jitter, max_delay);
		await wait(actual_delay); // eslint-disable-line no-await-in-loop

		// Exponential backoff
		delay = Math.min(delay * 1.5, max_delay);
	}

	throw new Error(`${pkg}@${version} not available after ${max_attempts} attempts`);
};

/**
 * Fetches package metadata from NPM registry.
 *
 * Returns name and latest version. Returns null if package doesn't exist
 * or registry is unreachable.
 *
 * @returns package info or null on error/not found
 */
export const get_package_info = async (
	pkg: string,
	options: {log?: Logger} = {},
): Promise<PackageInfo | null> => {
	const {log} = options;
	try {
		const result = await spawn_out('npm', ['view', pkg, '--json']);

		if (result.stdout) {
			const data = JSON.parse(result.stdout);
			return {
				name: data.name,
				version: data.version,
			};
		}

		return null;
	} catch (error) {
		log?.debug(`Failed to get package info for ${pkg}: ${error}`);
		return null;
	}
};

export const package_exists = async (
	pkg: string,
	options: {log?: Logger} = {},
): Promise<boolean> => {
	const info = await get_package_info(pkg, options);
	return info !== null;
};
