import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `circular_prod_deps_error` fixture's repos
const config: GitopsConfig = {
	repos: ['pkg_a', 'pkg_b']
};

export default config;
