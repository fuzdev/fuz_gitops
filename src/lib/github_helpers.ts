import { ensure_end } from '@fuzdev/fuz_util/string.ts';

import type { GithubPullRequest } from './github.ts';
import type { Repo } from './repo.svelte.ts';

export type FilterPullRequest = (pull_request: GithubPullRequest, repo: Repo) => boolean;

export interface PullRequestMeta {
	repo: Repo;
	pull_request: GithubPullRequest;
}

export const to_pull_requests = (
	repos: Array<Repo>,
	filter_pull_request?: FilterPullRequest
): Array<PullRequestMeta> =>
	repos.flatMap((repo) =>
		(repo.pull_requests ?? [])
			.filter((pull_request) => !filter_pull_request || filter_pull_request(pull_request, repo))
			.map((pull_request) => ({ repo, pull_request }))
	);

export const to_pull_url = (repo_url: string, pull: GithubPullRequest): string =>
	ensure_end(repo_url, '/') + 'pull/' + pull.number;
