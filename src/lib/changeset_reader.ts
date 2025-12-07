/**
 * Changeset parsing and version prediction from `.changeset/*.md` files.
 *
 * Reads changesets to determine which packages need publishing and their version bumps.
 * For auto-generating changesets during publishing, see `changeset_generator.ts`.
 */

import type {Logger} from '@fuzdev/fuz_util/log.js';
import {existsSync} from 'node:fs';
import {readdir, readFile} from 'node:fs/promises';
import {join} from 'node:path';

import type {LocalRepo} from './local_repo.js';
import type {BumpType} from './semver.js';
import {compare_bump_types, calculate_next_version} from './version_utils.js';

export interface ChangesetInfo {
	filename: string;
	packages: Array<{name: string; bump_type: BumpType}>;
	summary: string;
}

/**
 * Parses changeset content string from markdown format.
 *
 * Pure function for testability - no file I/O, just string parsing.
 * Extracts package names, bump types, and summary from YAML frontmatter format.
 * Returns null if format is invalid or no packages found.
 *
 * Expected format:
 * ---
 * "package-name": patch
 * "@scope/package": minor
 * ---
 *
 * Summary of changes
 *
 * @param content changeset markdown with YAML frontmatter
 * @param filename optional filename for error reporting context
 * @returns parsed changeset info or null if invalid format
 */
export const parse_changeset_content = (
	content: string,
	filename = 'changeset.md',
): ChangesetInfo | null => {
	// Match frontmatter between --- markers
	const frontmatter_match = /^---\s*\n([\s\S]*?)\n---\s*\n([\s\S]*)/.exec(content);
	if (!frontmatter_match) {
		return null;
	}

	const frontmatter = frontmatter_match[1]!;
	const summary = frontmatter_match[2]!.trim();

	// Parse package entries
	const packages: Array<{name: string; bump_type: BumpType}> = [];

	// Match lines like: "package-name": patch
	// or: '@scope/package': minor
	// Allow leading whitespace
	const package_regex = /^\s*["']([^"']+)["']\s*:\s*(major|minor|patch)\s*$/gm;
	let match;

	while ((match = package_regex.exec(frontmatter)) !== null) {
		packages.push({
			name: match[1]!,
			bump_type: match[2]! as BumpType,
		});
	}

	if (packages.length === 0) {
		return null;
	}

	return {
		filename,
		packages,
		summary,
	};
};

export const parse_changeset_file = async (
	filepath: string,
	log?: Logger,
): Promise<ChangesetInfo | null> => {
	try {
		const content = await readFile(filepath, 'utf8');
		const filename = filepath.split('/').pop() || '';

		const result = parse_changeset_content(content, filename);

		if (!result) {
			log?.warn(`  Invalid changeset format in ${filepath}`);
		}

		return result;
	} catch (error) {
		log?.error(`  Failed to parse changeset ${filepath}: ${error}`);
		return null;
	}
};

export const read_changesets = async (
	repo: LocalRepo,
	log?: Logger,
): Promise<Array<ChangesetInfo>> => {
	const changesets_dir = join(repo.repo_dir, '.changeset');

	try {
		const files = await readdir(changesets_dir);
		const changeset_files = files.filter((f) => f.endsWith('.md') && f !== 'README.md');

		const changesets: Array<ChangesetInfo> = [];

		for (const file of changeset_files) {
			const filepath = join(changesets_dir, file);
			const changeset = await parse_changeset_file(filepath, log); // eslint-disable-line no-await-in-loop
			if (changeset) {
				changesets.push(changeset);
			}
		}

		return changesets;
	} catch (_error) {
		// No .changeset directory or error reading
		return [];
	}
};

/**
 * Determines the bump type for a package from its changesets.
 *
 * When multiple changesets exist for the same package, returns the highest
 * bump type (major > minor > patch) to ensure the most significant change
 * is reflected in the version bump.
 *
 * @returns the highest bump type, or null if package has no changesets
 */
export const determine_bump_from_changesets = (
	changesets: Array<ChangesetInfo>,
	package_name: string,
): BumpType | null => {
	let highest_bump: BumpType | null = null;

	for (const changeset of changesets) {
		for (const pkg of changeset.packages) {
			if (pkg.name === package_name) {
				if (!highest_bump || compare_bump_types(pkg.bump_type, highest_bump) > 0) {
					highest_bump = pkg.bump_type;
				}
			}
		}
	}

	return highest_bump;
};

/**
 * Checks if a repo has any changeset files (excluding README.md).
 *
 * Used by preflight checks and publishing workflow to determine which packages
 * need to be published. Returns false if .changeset directory doesn't exist
 * or contains only README.md.
 *
 * @returns true if repo has unpublished changesets
 */
export const has_changesets = async (repo: LocalRepo): Promise<boolean> => {
	const changesets_dir = join(repo.repo_dir, '.changeset');
	if (!existsSync(changesets_dir)) {
		return false;
	}

	try {
		const files = await readdir(changesets_dir);
		// Look for markdown files that aren't the README
		return files.some((file) => file.endsWith('.md') && file !== 'README.md');
	} catch {
		return false;
	}
};

/**
 * Predicts the next version by analyzing all changesets in a repo.
 *
 * Reads all changesets, determines the highest bump type for the package,
 * and calculates the next version. Returns null if no changesets found.
 *
 * Critical for dry-run mode accuracy - allows simulating publishes without
 * actually running `gro publish` which consumes changesets.
 *
 * @returns predicted version and bump type, or null if no changesets
 */
export const predict_next_version = async (
	repo: LocalRepo,
	log?: Logger,
): Promise<{version: string; bump_type: BumpType} | null> => {
	const changesets = await read_changesets(repo, log);
	if (changesets.length === 0) {
		return null;
	}

	const bump_type = determine_bump_from_changesets(changesets, repo.library.name);
	if (!bump_type) {
		return null;
	}

	const current_version = repo.library.package_json.version || '0.0.0';
	const next_version = calculate_next_version(current_version, bump_type);

	log?.debug(
		`  Predicted ${repo.library.name}: ${current_version} → ${next_version} (${bump_type})`,
	);

	return {
		version: next_version,
		bump_type,
	};
};
