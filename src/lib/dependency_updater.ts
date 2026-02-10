import type {Logger} from '@fuzdev/fuz_util/log.js';
import {join} from 'node:path';
import type {FsOperations} from '@fuzdev/fuz_util/fs_operations.js';

import type {LocalRepo} from './local_repo.js';
import type {PublishedVersion} from './multi_repo_publisher.js';
import {
	create_changeset_for_dependency_updates,
	create_dependency_updates,
} from './changeset_generator.js';
import {needs_update, get_update_prefix} from './version_utils.js';
import type {GitOperations} from './operations.js';
import {default_git_operations, default_fs_operations} from './operations_defaults.js';

export type VersionStrategy = 'exact' | 'caret' | 'tilde' | 'gte';

export interface UpdatePackageJsonOptions {
	strategy?: VersionStrategy;
	published_versions?: Map<string, PublishedVersion>;
	log?: Logger;
	git_ops?: GitOperations;
	fs_ops?: FsOperations;
}

/**
 * Updates package.json dependencies and creates changeset if needed.
 *
 * Workflow:
 * 1. Updates all dependency types (dependencies, devDependencies, peerDependencies)
 * 2. Writes updated package.json with tabs formatting
 * 3. Creates auto-changeset if published_versions provided (for transitive updates)
 * 4. Commits both package.json and changeset with standard message
 *
 * Uses version strategy to determine prefix (exact, caret, tilde) while preserving
 * existing prefixes when possible.
 *
 * @throws {Error} if file operations or git operations fail
 */
export const update_package_json = async (
	repo: LocalRepo,
	updates: Map<string, string>,
	options: UpdatePackageJsonOptions = {},
): Promise<void> => {
	const {
		strategy = 'caret',
		published_versions,
		log,
		git_ops = default_git_operations,
		fs_ops = default_fs_operations,
	} = options;
	if (updates.size === 0) return;

	const package_json_path = join(repo.repo_dir, 'package.json');

	// Read current package.json
	const content_result = await fs_ops.read_file({path: package_json_path, encoding: 'utf8'});
	if (!content_result.ok) {
		throw new Error(`Failed to read package.json: ${content_result.message}`);
	}
	const package_json = JSON.parse(content_result.value);

	// Apply version strategy
	const prefix =
		strategy === 'exact' ? '' : strategy === 'caret' ? '^' : strategy === 'gte' ? '>=' : '~';

	let updated = false;

	// Update dependencies
	if (package_json.dependencies) {
		for (const [name, version] of updates) {
			if (name in package_json.dependencies) {
				const current = package_json.dependencies[name];
				const update_prefix = get_update_prefix(current, prefix);
				package_json.dependencies[name] = update_prefix + version;
				updated = true;
			}
		}
	}

	// Update devDependencies
	if (package_json.devDependencies) {
		for (const [name, version] of updates) {
			if (name in package_json.devDependencies) {
				const current = package_json.devDependencies[name];
				const update_prefix = get_update_prefix(current, prefix);
				package_json.devDependencies[name] = update_prefix + version;
				updated = true;
			}
		}
	}

	// Update peerDependencies
	if (package_json.peerDependencies) {
		for (const [name, version] of updates) {
			if (name in package_json.peerDependencies) {
				const current = package_json.peerDependencies[name];
				const update_prefix = get_update_prefix(current, prefix);
				package_json.peerDependencies[name] = update_prefix + version;
				updated = true;
			}
		}
	}

	if (!updated) return;

	// Write updated package.json
	const write_result = await fs_ops.write_file({
		path: package_json_path,
		content: JSON.stringify(package_json, null, '\t') + '\n',
	});
	if (!write_result.ok) {
		throw new Error(`Failed to write package.json: ${write_result.message}`);
	}

	// Create changeset if we have published version info
	if (published_versions && published_versions.size > 0) {
		// Build dependency updates info for changeset
		const all_deps: Map<string, string> = new Map();

		// Collect all current dependencies with their versions
		if (repo.dependencies) {
			for (const [name, version] of repo.dependencies) {
				if (updates.has(name)) {
					all_deps.set(name, version);
				}
			}
		}
		if (repo.dev_dependencies) {
			for (const [name, version] of repo.dev_dependencies) {
				if (updates.has(name)) {
					all_deps.set(name, version);
				}
			}
		}
		if (repo.peer_dependencies) {
			for (const [name, version] of repo.peer_dependencies) {
				if (updates.has(name)) {
					all_deps.set(name, version);
				}
			}
		}

		const dependency_updates = create_dependency_updates(all_deps, published_versions);

		if (dependency_updates.length > 0) {
			const changeset_path = await create_changeset_for_dependency_updates(
				repo,
				dependency_updates,
				{log},
			);

			// Add changeset to git
			const add_result = await git_ops.add({files: changeset_path, cwd: repo.repo_dir});
			if (!add_result.ok) {
				throw new Error(`Failed to stage changeset: ${add_result.message}`);
			}
		}
	}

	// Commit the changes (including both package.json and changeset)
	const add_pkg_result = await git_ops.add({files: 'package.json', cwd: repo.repo_dir});
	if (!add_pkg_result.ok) {
		throw new Error(`Failed to stage package.json: ${add_pkg_result.message}`);
	}

	const commit_result = await git_ops.commit({
		message: `update dependencies after publishing`,
		cwd: repo.repo_dir,
	});
	if (!commit_result.ok) {
		throw new Error(`Failed to commit: ${commit_result.message}`);
	}
};

export interface UpdateAllReposOptions {
	strategy?: VersionStrategy;
	log?: Logger;
	git_ops?: GitOperations;
	fs_ops?: FsOperations;
}

export const update_all_repos = async (
	repos: Array<LocalRepo>,
	published: Map<string, string>,
	options: UpdateAllReposOptions = {},
): Promise<{updated: number; failed: Array<{repo: string; error: Error}>}> => {
	const {
		strategy = 'caret',
		log,
		git_ops = default_git_operations,
		fs_ops = default_fs_operations,
	} = options;
	let updated_count = 0;
	const failed: Array<{repo: string; error: Error}> = [];

	for (const repo of repos) {
		const updates: Map<string, string> = new Map();

		// Find dependencies that were published
		if (repo.dependencies) {
			for (const [dep_name] of repo.dependencies) {
				const new_version = published.get(dep_name);
				if (new_version) {
					updates.set(dep_name, new_version);
				}
			}
		}

		if (repo.dev_dependencies) {
			for (const [dep_name] of repo.dev_dependencies) {
				const new_version = published.get(dep_name);
				if (new_version) {
					updates.set(dep_name, new_version);
				}
			}
		}

		if (repo.peer_dependencies) {
			for (const [dep_name] of repo.peer_dependencies) {
				const new_version = published.get(dep_name);
				if (new_version) {
					updates.set(dep_name, new_version);
				}
			}
		}

		if (updates.size === 0) continue;

		try {
			await update_package_json(repo, updates, {strategy, log, git_ops, fs_ops}); // eslint-disable-line no-await-in-loop
			updated_count++;
			log?.info(`    Updated ${updates.size} dependencies in ${repo.library.name}`);
		} catch (error) {
			const err = error instanceof Error ? error : new Error(String(error));
			failed.push({repo: repo.library.name, error: err});
			log?.error(`    Failed to update ${repo.library.name}: ${err.message}`);
		}
	}

	return {updated: updated_count, failed};
};

export const find_updates_needed = (
	repo: LocalRepo,
	published: Map<string, string>,
): Map<
	string,
	{current: string; new: string; type: 'dependencies' | 'devDependencies' | 'peerDependencies'}
> => {
	const updates: Map<
		string,
		{current: string; new: string; type: 'dependencies' | 'devDependencies' | 'peerDependencies'}
	> = new Map();

	// Check dependencies
	if (repo.dependencies) {
		for (const [dep_name, current_version] of repo.dependencies) {
			const new_version = published.get(dep_name);
			if (new_version && needs_update(current_version, new_version)) {
				updates.set(dep_name, {
					current: current_version,
					new: new_version,
					type: 'dependencies',
				});
			}
		}
	}

	// Check devDependencies
	if (repo.dev_dependencies) {
		for (const [dep_name, current_version] of repo.dev_dependencies) {
			const new_version = published.get(dep_name);
			if (new_version && needs_update(current_version, new_version)) {
				updates.set(dep_name, {
					current: current_version,
					new: new_version,
					type: 'devDependencies',
				});
			}
		}
	}

	// Check peerDependencies
	if (repo.peer_dependencies) {
		for (const [dep_name, current_version] of repo.peer_dependencies) {
			const new_version = published.get(dep_name);
			if (new_version && needs_update(current_version, new_version)) {
				updates.set(dep_name, {
					current: current_version,
					new: new_version,
					type: 'peerDependencies',
				});
			}
		}
	}

	return updates;
};
