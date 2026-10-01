import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `peer_deps_only` fixture's repos
const config: GitopsConfig = {
	repos: ['core', 'utils', 'plugin_a', 'plugin_b', 'adapter']
};

export default config;
