import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `circular_dev_deps` fixture's repos
const config: GitopsConfig = {
	repos: ['tool_a', 'tool_b', 'consumer']
};

export default config;
