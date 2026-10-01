import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `major_bumps` fixture's repos
const config: GitopsConfig = {
	repos: ['unstable', 'stable', 'app_using_unstable', 'app_using_stable', 'complex_app']
};

export default config;
