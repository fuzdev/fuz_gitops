// TODO: candidate for extraction to `@fuzdev/fuz_util`

import type {BumpType} from './semver.js';

export const is_wildcard = (version: string): boolean => {
	return version === '*';
};

/**
 * Strips version prefix (^, ~, >=, <=, etc) from a version string.
 */
export const strip_version_prefix = (version: string): string => {
	return version.replace(/^(>=|<=|>|<|=|\^|~)/, '');
};

/**
 * Gets the version prefix (^, ~, >=, <=, or empty string).
 */
export const get_version_prefix = (version: string): string => {
	const match = /^(>=|<=|>|<|=|\^|~)/.exec(version);
	return match ? match[1]! : '';
};

/**
 * Normalizes version string for comparison.
 *
 * Strips prefixes (^, ~, >=) to get bare version number.
 * Handles wildcards as-is. Used by needs_update to compare versions.
 *
 * @example
 * ```ts
 * normalize_version_for_comparison('^1.2.3') // '1.2.3'
 * ```
 * @example
 * ```ts
 * normalize_version_for_comparison('>=2.0.0') // '2.0.0'
 * ```
 * @example
 * ```ts
 * normalize_version_for_comparison('*') // '*'
 * ```
 */
export const normalize_version_for_comparison = (version: string): string => {
	// Handle wildcards
	if (is_wildcard(version)) return version;

	// Handle >= ranges - extract just the version number
	if (version.startsWith('>=')) {
		return version.substring(2).trim();
	}

	// Strip other prefixes
	return strip_version_prefix(version);
};

export const needs_update = (current: string, new_version: string): boolean => {
	// Always update wildcards
	if (is_wildcard(current)) return true;

	// Compare normalized versions
	const current_normalized = normalize_version_for_comparison(current);
	const new_normalized = normalize_version_for_comparison(new_version);

	return current_normalized !== new_normalized;
};

/**
 * Determines version prefix to use when updating dependencies.
 *
 * Strategy:
 * - Wildcard (*): Use caret (^) as default
 * - Has existing prefix: Preserve it (^, ~, >=, <=, etc)
 * - No prefix: Use default_strategy
 *
 * This preserves user intent while handling wildcard replacements sensibly.
 *
 * @param default_strategy prefix to use when no existing prefix found
 */
export const get_update_prefix = (
	current_version: string,
	default_strategy: '^' | '~' | '' | '>=' = '^',
): string => {
	// Use caret for wildcard replacements
	if (is_wildcard(current_version)) {
		return '^';
	}

	// Preserve existing prefix if present
	const existing_prefix = get_version_prefix(current_version);
	if (existing_prefix) {
		return existing_prefix;
	}

	// Use default strategy
	return default_strategy;
};

/**
 * Determines if a bump is a breaking change based on semver rules.
 * Pre-1.0: minor bumps are breaking
 * 1.0+: major bumps are breaking
 */
export const is_breaking_change = (
	old_version: string,
	bump_type: 'major' | 'minor' | 'patch',
): boolean => {
	const [major] = old_version.split('.').map(Number);
	const is_pre_1_0 = major === 0;

	if (is_pre_1_0) {
		// In 0.x.x, minor bumps are breaking changes
		return bump_type === 'minor' || bump_type === 'major';
	} else {
		// In 1.x.x+, only major bumps are breaking
		return bump_type === 'major';
	}
};

export const detect_bump_type = (
	old_version: string,
	new_version: string,
): 'major' | 'minor' | 'patch' => {
	const old_parts = old_version.split('.').map(Number);
	const new_parts = new_version.split('.').map(Number);

	if (new_parts[0]! > old_parts[0]!) return 'major';
	if (new_parts[1]! > old_parts[1]!) return 'minor';
	return 'patch';
};

/**
 * Compares bump types. Returns positive if a > b, negative if a < b, 0 if equal.
 */
export const compare_bump_types = (a: BumpType, b: BumpType): number => {
	const order: Record<BumpType, number> = {
		major: 3,
		minor: 2,
		patch: 1,
	};
	return order[a] - order[b];
};

export const calculate_next_version = (current_version: string, bump_type: BumpType): string => {
	const parts = current_version.split('.').map(Number);
	if (parts.length !== 3 || parts.some((p) => Number.isNaN(p))) {
		throw new Error(`Invalid version format: ${current_version}`);
	}

	const [major, minor, patch] = parts;

	switch (bump_type) {
		case 'major':
			return `${major! + 1}.0.0`;
		case 'minor':
			return `${major!}.${minor! + 1}.0`;
		case 'patch':
			return `${major!}.${minor!}.${patch! + 1}`;
		default:
			throw new Error(`Invalid bump type: ${bump_type}`);
	}
};
