import type {Logger} from '@fuzdev/fuz_util/log.js';
import {z} from 'zod';
import {fetch_value, type FetchValueCache} from '@fuzdev/fuz_util/fetch.js';

/**
 * Minimal interface for GitHub API calls - works with both Pkg and Repo.
 */
export interface GithubRepoInfo {
	owner_name: string | null;
	repo_name: string;
}

/**
 * @see https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28#list-pull-requests
 */
export const GithubPullRequest = z.object({
	number: z.number(),
	title: z.string(),
	user: z.object({
		login: z.string(),
	}),
	draft: z.boolean(),
});
export type GithubPullRequest = z.infer<typeof GithubPullRequest>;
export const GithubPullRequests = z.array(GithubPullRequest);
export type GithubPullRequests = z.infer<typeof GithubPullRequests>;

/**
 * @see https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28#list-pull-requests
 */
export const fetch_github_pull_requests = async (
	repo_info: GithubRepoInfo,
	options: {
		cache?: FetchValueCache;
		log?: Logger;
		token?: string;
		api_version?: string;
	} = {},
): Promise<GithubPullRequests | null> => {
	const {cache, log, token, api_version} = options;
	if (!repo_info.owner_name) throw Error('owner_name is required');
	const headers = api_version ? new Headers({'x-github-api-version': api_version}) : undefined;
	const url = `https://api.github.com/repos/${repo_info.owner_name}/${repo_info.repo_name}/pulls`;
	const fetched = await fetch_value(url, {
		request: {headers},
		parse: GithubPullRequests.parse,
		token,
		cache,
		log,
	});
	if (!fetched.ok) {
		// TODO @many this is messy but I think it's the main case we need to worry about?
		if (fetched.status === 401) {
			throw Error(
				'401 response fetching github pull requests - check your SECRET_GITHUB_API_TOKEN',
			);
		}
		return null;
	}
	return fetched.value;
};

/**
 * @see https://docs.github.com/en/rest/checks/runs?apiVersion=2022-11-28#list-check-runs-for-a-git-reference
 */
export const GithubCheckRunsItem = z.object({
	status: z.enum(['queued', 'in_progress', 'completed']),
	conclusion: z
		.enum(['success', 'failure', 'neutral', 'cancelled', 'skipped', 'timed_out', 'action_required'])
		.nullable(),
});
export type GithubCheckRunsItem = z.infer<typeof GithubCheckRunsItem>;
export const GithubCheckRuns = z.object({
	total_count: z.number(),
	check_runs: z.array(GithubCheckRunsItem),
});
export type GithubCheckRuns = z.infer<typeof GithubCheckRuns>;

/**
 * @see https://docs.github.com/en/rest/checks/runs?apiVersion=2022-11-28#list-check-runs-for-a-git-reference
 */
export const fetch_github_check_runs = async (
	repo_info: GithubRepoInfo,
	options: {
		cache?: FetchValueCache;
		log?: Logger;
		token?: string;
		api_version?: string;
		ref?: string;
	} = {},
): Promise<GithubCheckRunsItem | null> => {
	const {cache, log, token, api_version, ref = 'main'} = options;
	if (!repo_info.owner_name) throw Error('owner_name is required');
	const headers = api_version ? new Headers({'x-github-api-version': api_version}) : undefined;
	const url = `https://api.github.com/repos/${repo_info.owner_name}/${repo_info.repo_name}/commits/${ref}/check-runs`;
	const fetched = await fetch_value(url, {
		request: {headers},
		parse: (v) => reduce_check_runs(GithubCheckRuns.parse(v).check_runs),
		token,
		cache,
		log,
	});
	if (!fetched.ok) {
		// TODO @many this is messy but I think it's the main case we need to worry about?
		if (fetched.status === 401) {
			throw Error('401 response fetching github CI status - check your SECRET_GITHUB_API_TOKEN');
		}
		return null;
	}
	return fetched.value;
};

const reduce_check_runs = (check_runs: Array<GithubCheckRunsItem>): GithubCheckRunsItem | null => {
	if (!check_runs.length) return null;
	// TODO fix these types and remove the eslint disable, `GithubCheckRunsItem` should maybe have nullable properties?
	let status!: GithubCheckRunsItem['status'];
	let conclusion!: GithubCheckRunsItem['conclusion'];
	for (const check_run of check_runs) {
		// eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
		if (!status || status === 'completed') {
			status = check_run.status;
		}
		if (!conclusion || conclusion === 'success') {
			conclusion = check_run.conclusion;
		}
	}
	return {status, conclusion};
};
