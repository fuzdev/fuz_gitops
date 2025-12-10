import type {Logger} from '@fuzdev/fuz_util/log.js';
import {wait} from '@fuzdev/fuz_util/async.js';
import type {FetchValueCache} from '@fuzdev/fuz_util/fetch.js';

import {fetch_github_check_runs, fetch_github_pull_requests} from './github.js';
import type {RepoJson} from './repo.svelte.js';
import type {LocalRepo} from './local_repo.js';

/* eslint-disable no-await-in-loop */

/**
 * Fetches GitHub metadata (CI status, PRs) for all repos.
 *
 * Fetches sequentially with delay between requests to respect GitHub API rate limits.
 * Uses `await_in_loop` intentionally to avoid parallel requests overwhelming the API.
 *
 * Error handling: Logs fetch failures but continues processing remaining repos.
 * Repos with failed fetches will have `null` for check_runs or pull_requests.
 *
 * @param delay milliseconds between API requests (default: 33ms)
 * @param cache optional cache from fuz_util's fetch.js for response memoization
 * @returns array of Repo objects with GitHub metadata attached
 */
export const fetch_repo_data = async (
	resolved_repos: Array<LocalRepo>,
	token?: string,
	cache?: FetchValueCache,
	log?: Logger,
	delay = 33,
	github_api_version?: string,
): Promise<Array<RepoJson>> => {
	const repos: Array<RepoJson> = [];
	for (const {library, library_json, repo_config} of resolved_repos) {
		const repo_url = library.repo_url;

		// CI status
		await wait(delay);
		const check_runs = await fetch_github_check_runs(library, {
			cache,
			log,
			token,
			api_version: github_api_version,
			ref: repo_config.branch,
		});
		if (!check_runs) log?.error('failed to fetch CI status: ' + repo_url);

		// pull requests
		await wait(delay);
		const pull_requests = await fetch_github_pull_requests(library, {
			cache,
			log,
			token,
			api_version: github_api_version,
		});
		if (!pull_requests) log?.error('failed to fetch issues: ' + repo_url);

		repos.push({
			library_json,
			check_runs,
			pull_requests,
		});
	}
	return repos;
};
