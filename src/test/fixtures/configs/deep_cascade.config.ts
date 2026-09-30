import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `deep_cascade` fixture's repos
const config: GitopsConfig = {
	repos: ['leaf', 'branch', 'trunk', 'root']
};

export default config;
