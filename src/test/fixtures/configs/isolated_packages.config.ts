import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `isolated_packages` fixture's repos
const config: GitopsConfig = {
	repos: ['util_a', 'util_b', 'util_c', 'util_d']
};

export default config;
