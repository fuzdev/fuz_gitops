import type { GitopsConfig } from '$lib/gitops_config.ts';

// the registry keys of the `three_way_dev_cycle` fixture's repos
const config: GitopsConfig = {
	repos: ['tool_x', 'tool_y', 'tool_z', 'app']
};

export default config;
