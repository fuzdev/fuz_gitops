import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `basic_publishing` fixture's repos
const config: GitopsConfig = {
	repos: ['repo_a', 'repo_b', 'repo_c', 'repo_d', 'repo_e']
};

export default config;
