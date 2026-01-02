/**
 * Helper functions for publishing plan calculations.
 *
 * Extracted from publishing_plan.ts to reduce file size.
 *
 * @module
 */

import type {LocalRepo} from './local_repo.js';
import type {BumpType} from './semver.js';
import {needs_update} from './version_utils.js';
import type {DependencyUpdate} from './publishing_plan.js';

/**
 * Calculates all dependency updates between packages based on predicted versions.
 *
 * Iterates through all repos, checking prod, peer, and dev dependencies to find
 * which packages will need dependency version bumps after publishing.
 *
 * Also tracks "breaking cascades" - when a breaking change propagates to dependents.
 */
export const calculate_dependency_updates = (
	repos: Array<LocalRepo>,
	predicted_versions: Map<string, string>,
	breaking_packages: Set<string>,
): {
	dependency_updates: Array<DependencyUpdate>;
	breaking_cascades: Map<string, Array<string>>;
} => {
	const dependency_updates: Array<DependencyUpdate> = [];
	const breaking_cascades: Map<string, Array<string>> = new Map();

	for (const repo of repos) {
		// Check prod dependencies
		if (repo.dependencies) {
			for (const [dep_name, current_version] of repo.dependencies) {
				const new_version = predicted_versions.get(dep_name);
				if (new_version && needs_update(current_version, new_version)) {
					dependency_updates.push({
						dependent_package: repo.library.name,
						updated_dependency: dep_name,
						current_version,
						new_version,
						type: 'dependencies',
						causes_republish: true,
					});

					if (breaking_packages.has(dep_name)) {
						const cascades = breaking_cascades.get(dep_name) || [];
						if (!cascades.includes(repo.library.name)) {
							cascades.push(repo.library.name);
						}
						breaking_cascades.set(dep_name, cascades);
					}
				}
			}
		}

		// Check peer dependencies
		if (repo.peer_dependencies) {
			for (const [dep_name, current_version] of repo.peer_dependencies) {
				const new_version = predicted_versions.get(dep_name);
				if (new_version && needs_update(current_version, new_version)) {
					dependency_updates.push({
						dependent_package: repo.library.name,
						updated_dependency: dep_name,
						current_version,
						new_version,
						type: 'peerDependencies',
						causes_republish: true,
					});

					if (breaking_packages.has(dep_name)) {
						const cascades = breaking_cascades.get(dep_name) || [];
						if (!cascades.includes(repo.library.name)) {
							cascades.push(repo.library.name);
						}
						breaking_cascades.set(dep_name, cascades);
					}
				}
			}
		}

		// Check dev dependencies
		if (repo.dev_dependencies) {
			for (const [dep_name, current_version] of repo.dev_dependencies) {
				const new_version = predicted_versions.get(dep_name);
				if (new_version && needs_update(current_version, new_version)) {
					dependency_updates.push({
						dependent_package: repo.library.name,
						updated_dependency: dep_name,
						current_version,
						new_version,
						type: 'devDependencies',
						causes_republish: false,
					});
				}
			}
		}
	}

	return {dependency_updates, breaking_cascades};
};

/**
 * Determines the required bump type for a package based on its dependency updates.
 *
 * Returns null if no prod/peer dependency updates, otherwise returns the minimum
 * required bump type (major for breaking deps, patch otherwise).
 *
 * Respects pre-1.0 semver conventions (minor for breaking in 0.x).
 */
export const get_required_bump_for_dependencies = (
	repo: LocalRepo,
	dependency_updates: Array<DependencyUpdate>,
	breaking_packages: Set<string>,
): BumpType | null => {
	// Check if this repo has any prod/peer dependency updates
	const relevant_updates = dependency_updates.filter(
		(update) =>
			update.dependent_package === repo.library.name &&
			(update.type === 'dependencies' || update.type === 'peerDependencies'),
	);

	if (relevant_updates.length === 0) {
		return null;
	}

	// Check if any of these dependencies have breaking changes
	const has_breaking_deps = relevant_updates.some((update) =>
		breaking_packages.has(update.updated_dependency),
	);

	const current_version = repo.library.package_json.version || '0.0.0';
	const [major] = current_version.split('.').map(Number);
	const is_pre_1_0 = major === 0;

	if (has_breaking_deps) {
		// Breaking changes propagate
		// Pre-1.0: use minor for breaking changes
		// 1.0+: use major for breaking changes
		return is_pre_1_0 ? 'minor' : 'major';
	}

	// For non-breaking dependency updates, use patch
	return 'patch';
};
