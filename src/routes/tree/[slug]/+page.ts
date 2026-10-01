import type { EntryGenerator } from './$types.js';

import { Repo, type RepoJson, repos_parse } from '$lib/repo.svelte.ts';
import pkg_json from 'virtual:pkg.json';

import { repos_json } from '$routes/repos.ts';

const parsed = repos_parse(
	repos_json.map((r: RepoJson) => new Repo(r)),
	pkg_json.homepage!
);

export const entries: EntryGenerator = () => {
	return parsed.repos.map((d) => ({ slug: d.repo_name }));
};
