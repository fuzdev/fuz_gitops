import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `private_packages` fixture's repos
const config: GitopsConfig = {
	repos: ['public_lib', 'private_tool', 'consumer']
};

export default config;
