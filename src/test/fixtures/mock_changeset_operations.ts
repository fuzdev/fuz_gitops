import type {Logger} from '@fuzdev/fuz_util/log.js';
import type {ChangesetOperations} from '$lib/operations.js';
import type {LocalRepo} from '$lib/local_repo.js';
import {parse_changeset_content, type ChangesetInfo} from '$lib/changeset_reader.js';
import {compare_bump_types, calculate_next_version} from '$lib/version_utils.js';
import type {BumpType} from '$lib/semver.js';
import type {RepoFixtureSet} from './repo_fixture_types.js';

/* eslint-disable @typescript-eslint/require-await */

/**
 * Create mock changeset operations that read from fixture data.
 */
export const create_mock_changeset_ops = (fixture: RepoFixtureSet): ChangesetOperations => {
	// Create lookup map for quick access
	const repos_by_name = new Map(fixture.repos.map((repo) => [repo.package_json.name, repo]));

	return {
		has_changesets: async (options: {repo: LocalRepo}) => {
			const {repo} = options;
			const fixture_repo = repos_by_name.get(repo.library.name);
			const value = !!(fixture_repo?.changesets && fixture_repo.changesets.length > 0);
			return {ok: true, value};
		},

		read_changesets: async (options: {repo: LocalRepo; log?: Logger}) => {
			const {repo} = options;
			const fixture_repo = repos_by_name.get(repo.library.name);
			if (!fixture_repo?.changesets) {
				return {ok: true, value: []};
			}

			const changesets: Array<ChangesetInfo> = [];
			for (const changeset_data of fixture_repo.changesets) {
				const parsed = parse_changeset_content(changeset_data.content, changeset_data.filename);
				if (parsed) {
					changesets.push(parsed);
				}
			}

			return {ok: true, value: changesets};
		},

		predict_next_version: async (options: {repo: LocalRepo; log?: Logger}) => {
			const {repo} = options;
			const fixture_repo = repos_by_name.get(repo.library.name);
			if (!fixture_repo?.changesets || fixture_repo.changesets.length === 0) {
				return null;
			}

			// Parse all changesets for this repo
			let highest_bump: BumpType | null = null;

			for (const changeset_data of fixture_repo.changesets) {
				const parsed = parse_changeset_content(changeset_data.content, changeset_data.filename);
				if (!parsed) continue;

				// Find the package entry for this repo
				const pkg_entry = parsed.packages.find((p) => p.name === repo.library.name);
				if (!pkg_entry) continue;

				// Track the highest bump type
				if (!highest_bump || compare_bump_types(pkg_entry.bump_type, highest_bump) > 0) {
					highest_bump = pkg_entry.bump_type;
				}
			}

			if (!highest_bump) {
				return null;
			}

			const current_version = fixture_repo.package_json.version;
			const new_version = calculate_next_version(current_version, highest_bump);

			return {ok: true, version: new_version, bump_type: highest_bump};
		},
	};
};
