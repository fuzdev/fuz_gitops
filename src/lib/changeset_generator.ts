/**
 * Auto-generation of changesets for dependency updates during publishing.
 *
 * Creates changesets when packages need to republish due to updated dependencies.
 * For parsing existing changesets, see `changeset_reader.ts`.
 *
 * @module
 */

import {join} from 'node:path';
import type {Logger} from '@fuzdev/fuz_util/log.js';
import type {LocalRepo} from './local_repo.js';
import type {PublishedVersion} from './multi_repo_publisher.js';
import {strip_version_prefix} from './version_utils.js';
import type {FsOperations} from './operations.js';
import {default_fs_operations} from './operations_defaults.js';

export interface DependencyVersionChange {
	package_name: string;
	from_version: string;
	to_version: string;
	bump_type: 'major' | 'minor' | 'patch';
	breaking: boolean;
}

/**
 * Creates a changeset file for dependency updates.
 * Returns the path to the created changeset file.
 */
export const create_changeset_for_dependency_updates = async (
	repo: LocalRepo,
	updates: Array<DependencyVersionChange>,
	options: {log?: Logger; fs_ops?: FsOperations} = {},
): Promise<string> => {
	const {log, fs_ops = default_fs_operations} = options;
	const changesets_dir = join(repo.repo_dir, '.changeset');

	// Ensure .changeset directory exists
	if (!fs_ops.exists({path: changesets_dir})) {
		const mkdir_result = await fs_ops.mkdir({path: changesets_dir, recursive: true});
		if (!mkdir_result.ok) {
			throw new Error(`Failed to create .changeset directory: ${mkdir_result.message}`);
		}
	}

	// Generate a unique filename
	const timestamp = Date.now();
	const random = Math.random().toString(36).substring(2, 8);
	const filename = `dependency-update-${timestamp}-${random}.md`;
	const filepath = join(changesets_dir, filename);

	// Determine the required bump type based on updates
	const required_bump = calculate_required_bump(repo, updates);

	// Generate changeset content
	const content = generate_changeset_content(repo.library.name, updates, required_bump);

	// Write the changeset file
	const write_result = await fs_ops.writeFile({path: filepath, content});
	if (!write_result.ok) {
		throw new Error(`Failed to write changeset file: ${write_result.message}`);
	}

	log?.info(`  Created changeset: ${filename}`);

	return filepath;
};

const calculate_required_bump = (
	repo: LocalRepo,
	updates: Array<DependencyVersionChange>,
): 'major' | 'minor' | 'patch' => {
	const current_version = repo.library.package_json.version || '0.0.0';
	const [major] = current_version.split('.').map(Number);
	const is_pre_1_0 = major === 0;

	// Check if any dependency had breaking changes
	const has_breaking = updates.some((u) => u.breaking);

	if (has_breaking) {
		// Breaking changes propagate
		// Pre-1.0: use minor for breaking changes
		// 1.0+: use major for breaking changes
		return is_pre_1_0 ? 'minor' : 'major';
	}

	// For non-breaking dependency updates, always use patch
	return 'patch';
};

/**
 * Generates markdown changeset content for dependency updates.
 *
 * Creates properly formatted changeset with YAML frontmatter, summary,
 * and categorized list of breaking vs regular updates. Output format
 * matches changesets CLI for consistency.
 *
 * @param package_name package receiving the dependency updates
 * @param updates list of dependency changes with version info
 * @param bump_type required bump type (calculated from breaking changes)
 * @returns markdown content ready to write to .changeset/*.md file
 */
export const generate_changeset_content = (
	package_name: string,
	updates: Array<DependencyVersionChange>,
	bump_type: 'major' | 'minor' | 'patch',
): string => {
	// Group updates by type
	const breaking_updates = updates.filter((u) => u.breaking);
	const regular_updates = updates.filter((u) => !u.breaking);

	let message = 'Update dependencies';

	if (breaking_updates.length > 0) {
		message = 'Update dependencies (BREAKING CHANGES)';
	}

	const lines: Array<string> = ['---', `"${package_name}": ${bump_type}`, '---', '', message, ''];

	if (breaking_updates.length > 0) {
		lines.push('Breaking dependency changes:');
		for (const update of breaking_updates) {
			lines.push(
				`- ${update.package_name}: ${update.from_version} → ${update.to_version} (${update.bump_type})`,
			);
		}
		lines.push('');
	}

	if (regular_updates.length > 0) {
		if (breaking_updates.length > 0) {
			lines.push('Other dependency updates:');
		} else {
			lines.push('Updated dependencies:');
		}
		for (const update of regular_updates) {
			lines.push(
				`- ${update.package_name}: ${update.from_version} → ${update.to_version} (${update.bump_type})`,
			);
		}
		lines.push('');
	}

	return lines.join('\n');
};

export const create_dependency_updates = (
	dependencies: Map<string, string>,
	published_versions: Map<string, PublishedVersion>,
): Array<DependencyVersionChange> => {
	const updates: Array<DependencyVersionChange> = [];

	for (const [dep_name, current_version] of dependencies) {
		const published = published_versions.get(dep_name);
		if (published) {
			// Strip version prefix (^, ~, etc)
			const clean_current = strip_version_prefix(current_version);

			updates.push({
				package_name: dep_name,
				from_version: clean_current,
				to_version: published.new_version,
				bump_type: published.bump_type,
				breaking: published.breaking,
			});
		}
	}

	return updates;
};
