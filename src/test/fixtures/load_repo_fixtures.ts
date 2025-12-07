/**
 * Utilities for loading in-memory fixture data as LocalRepo objects.
 */

import type {LibraryJson} from '@fuzdev/fuz_util/library_json.js';
import {Library} from '@fuzdev/fuz_ui/library.svelte.js';

import type {LocalRepo} from '$lib/local_repo.js';
import type {RepoFixtureSet, RepoFixtureData} from './repo_fixture_types.js';

/**
 * Convert fixture data to LocalRepo objects that can be used with publishing functions.
 */
export const fixture_to_local_repos = (fixture: RepoFixtureSet): Array<LocalRepo> => {
	return fixture.repos.map((repo_data) => fixture_repo_to_local_repo(repo_data));
};

/**
 * Convert a single fixture repo to a LocalRepo object.
 */
export const fixture_repo_to_local_repo = (repo_data: RepoFixtureData): LocalRepo => {
	const {repo_name, repo_url, package_json} = repo_data;

	// Create LibraryJson from fixture data
	const library_json: LibraryJson = {
		name: package_json.name,
		repo_name,
		repo_url,
		owner_name: 'test',
		homepage_url: `https://test.com/${repo_name}`,
		logo_url: null,
		logo_alt: `logo for ${repo_name}`,
		npm_url: null,
		changelog_url: null,
		published: false,
		package_json,
		source_json: {
			name: package_json.name,
			version: package_json.version,
			modules: [],
		},
	};

	const library = new Library(library_json);

	const local_repo: LocalRepo = {
		library,
		library_json,
		repo_dir: `/fixtures/${repo_name}`, // Fake path - not used in tests
		repo_git_ssh_url: `git@github.com:test/${repo_name}.git`,
		repo_config: {
			repo_url,
			repo_dir: null,
			branch: 'main',
		},
	};

	// Add dependency maps if present
	if (package_json.dependencies) {
		local_repo.dependencies = new Map(Object.entries(package_json.dependencies));
	}
	if (package_json.devDependencies) {
		local_repo.dev_dependencies = new Map(Object.entries(package_json.devDependencies));
	}
	if (package_json.peerDependencies) {
		local_repo.peer_dependencies = new Map(Object.entries(package_json.peerDependencies));
	}

	return local_repo;
};
