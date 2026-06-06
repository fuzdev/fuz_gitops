import {create_context} from '@fuzdev/fuz_ui/context_helpers.js';
import type {LibraryJson, SourceJson} from '@fuzdev/fuz_util/library_json.js';
import type {PkgJson} from '@fuzdev/fuz_util/pkg_json.js';
import type {PackageJson} from '@fuzdev/fuz_util/package_json.js';
import type {Url} from '@fuzdev/fuz_util/url.js';
import {Library} from '@fuzdev/fuz_ui/library.svelte.js';

import {GithubCheckRunsItem, type GithubPullRequest} from './github.js';

/**
 * Serialized repo data as stored in `repos.ts` (JSON).
 *
 * `package_json` is the repo's full `package.json`, carried alongside
 * `library_json` because gitops reads `dependencies`/`devDependencies` for the
 * publishing cascade — fields the curated `LibraryJson.pkg_json` omits.
 */
export interface RepoJson {
	library_json: LibraryJson;
	package_json: PackageJson;
	check_runs: GithubCheckRunsItem | null;
	pull_requests: Array<GithubPullRequest> | null;
}

/**
 * Runtime repo with `Library` composition for package metadata.
 *
 * Wraps a `Library` instance and adds GitHub-specific data (CI status, PRs).
 * Convenience getters delegate to `this.library.*` for common properties.
 */
export class Repo {
	readonly library: Library;
	/** The repo's full `package.json` (with `dependencies`/`devDependencies`). */
	readonly package_json: PackageJson;
	check_runs: GithubCheckRunsItem | null;
	pull_requests: Array<GithubPullRequest> | null;

	// Convenience getters delegating to library
	get name(): string {
		return this.library.name;
	}
	get repo_name(): string {
		return this.library.repo_name;
	}
	get repo_url(): Url {
		return this.library.repo_url;
	}
	get homepage_url(): Url | null {
		return this.library.homepage_url;
	}
	get logo_url(): Url | null {
		return this.library.logo_url;
	}
	get logo_alt(): string {
		return this.library.logo_alt;
	}
	get npm_url(): Url | null {
		return this.library.npm_url;
	}
	get changelog_url(): Url | null {
		return this.library.changelog_url;
	}
	/** Curated package identity, delegating to `library`. Distinct from the full `package_json`. */
	get pkg_json(): PkgJson {
		return this.library.pkg_json;
	}
	get source_json(): SourceJson {
		return this.library.source_json;
	}

	constructor(repo_json: RepoJson) {
		this.library = new Library(repo_json.library_json);
		this.package_json = repo_json.package_json;
		this.check_runs = repo_json.check_runs;
		this.pull_requests = repo_json.pull_requests;
	}
}

export interface Repos {
	repo: Repo;
	repos: Array<Repo>;
}

export const repos_context = create_context<Repos>();

export const repos_parse = (repos: Array<Repo>, homepage_url: string): Repos => {
	// We expect to find this because it's sourced from the local package.json
	const repo = repos.find((d) => d.homepage_url === homepage_url);
	if (!repo) throw Error(`Cannot find repo with homepage_url: ${homepage_url}`);

	return {repo, repos};
};
