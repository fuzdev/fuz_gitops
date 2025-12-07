import {ensure_end} from '@fuzdev/fuz_util/string.js';

import type {GithubPullRequest} from './github.js';
import type {Repo} from './repo.svelte.js';

export type FilterPullRequest = (pull_request: GithubPullRequest, repo: Repo) => boolean;

export interface PullRequestMeta {
	repo: Repo;
	pull_request: GithubPullRequest;
}

export const to_pull_requests = (
	repos: Array<Repo>,
	filter_pull_request?: FilterPullRequest,
): Array<PullRequestMeta> =>
	repos
		.flatMap((repo) => {
			if (!repo.pull_requests) return null;
			// TODO hacky, figure out the data structure
			return repo.pull_requests.map((pull_request) =>
				repo.package_json.homepage &&
				(!filter_pull_request || filter_pull_request(pull_request, repo))
					? {repo, pull_request}
					: null,
			);
		})
		.filter((v) => v !== null);

export const to_pull_url = (repo_url: string, pull: GithubPullRequest): string =>
	ensure_end(repo_url, '/') + 'pull/' + pull.number;
