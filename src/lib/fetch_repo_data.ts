import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { wait } from '@fuzdev/fuz_util/async.ts';
import type { FetchValueCache } from '@fuzdev/fuz_util/fetch.ts';

import {
	fetch_github_check_runs,
	fetch_github_pull_requests,
	type GithubCheckRunsItem
} from './github.ts';
import type { RepoJson } from './repo.svelte.ts';
import type { LocalRepo } from './local_repo.ts';

/**
 * Options for `fetch_repo_data`.
 */
export interface FetchRepoDataOptions {
	/** The repos to fetch GitHub metadata for, in order. */
	local_repos: Array<LocalRepo>;
	/** GitHub API token, sent with each request. */
	token?: string;
	/** Response memoization, from `fuz_util`'s `fetch.ts`. */
	cache?: FetchValueCache;
	log?: Logger;
	/** Milliseconds to wait before each API request. Defaults to `33`. */
	delay?: number;
	/** Sent as the `x-github-api-version` header when set. */
	github_api_version?: string;
	/** The `fetch` the API requests go through. Defaults to `globalThis.fetch`. */
	fetch?: typeof globalThis.fetch;
}

/**
 * Fetches GitHub metadata (CI status, PRs) for all repos.
 *
 * Fetches sequentially with a delay before each request to respect GitHub API
 * rate limits. Uses `await_in_loop` intentionally to avoid parallel requests
 * overwhelming the API. CI status is read for the branch each repo's registry
 * entry follows (`main` when it names none), and that branch is written to
 * `RepoJson.branch`.
 *
 * A repo whose registry entry declares no CI (`ci: false`) isn't asked for
 * check runs, and one with none on its branch isn't a failure: both leave
 * `check_runs` `null` without logging. A failed fetch is logged as an error
 * and leaves `null` for that repo's `check_runs` or `pull_requests`, and the
 * remaining repos still fetch — except a 401 response or a repo URL without a
 * GitHub owner, which throw.
 *
 * @param options - the repos, credentials, cache, and pacing
 * @returns a `RepoJson` for each repo, in the order given
 * @throws {Error} on a 401 response (check `SECRET_GITHUB_API_TOKEN`) or a
 *   repo whose URL has no GitHub owner
 */
export const fetch_repo_data = async (options: FetchRepoDataOptions): Promise<Array<RepoJson>> => {
	const { local_repos, token, cache, log, delay = 33, github_api_version, fetch } = options;
	const repos: Array<RepoJson> = [];
	for (const { library, package_json, entry } of local_repos) {
		const repo_url = library.repo_url;
		const branch = entry.branch ?? 'main';

		// CI status, for repos the registry says run CI
		let check_runs: GithubCheckRunsItem | null = null;
		if (entry.ci) {
			await wait(delay);
			const fetched = await fetch_github_check_runs(library, {
				cache,
				log,
				token,
				api_version: github_api_version,
				ref: branch,
				fetch
			});
			if (fetched.ok) {
				check_runs = fetched.value;
				if (!check_runs) log?.debug(`no check runs on \`${branch}\`: ${repo_url}`);
			} else {
				log?.error(`failed to fetch CI status (${fetched.status}): ${repo_url}`);
			}
		}

		// pull requests
		await wait(delay);
		const pull_requests = await fetch_github_pull_requests(library, {
			cache,
			log,
			token,
			api_version: github_api_version,
			fetch
		});
		if (!pull_requests) log?.error('failed to fetch pull requests: ' + repo_url);

		repos.push({
			library_json: library.library_json,
			package_json,
			branch,
			check_runs,
			pull_requests
		});
	}
	return repos;
};
