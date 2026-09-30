import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `multiple_dep_types` fixture's repos
const config: GitopsConfig = {
	repos: ['core', 'plugin', 'adapter']
};

export default config;
