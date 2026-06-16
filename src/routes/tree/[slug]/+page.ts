import type {EntryGenerator} from './$types.js';

import {Repo, type RepoJson, repos_parse} from '$lib/repo.svelte.ts';
import {repos_json} from '$routes/repos.ts';

const parsed = repos_parse(
	repos_json.map((r: RepoJson) => new Repo(r)),
	'https://gitops.fuz.dev/',
);

export const entries: EntryGenerator = () => {
	return parsed.repos.map((d) => ({slug: d.repo_name}));
};
