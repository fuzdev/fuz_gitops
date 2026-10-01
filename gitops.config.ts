import type { GitopsConfig } from './src/lib/gitops_config.ts';

// repos.toml registry keys; everything else about each repo comes from the registry
const config: GitopsConfig = {
	repos: [
		'fuz_app',
		'fuz_css',
		'fuz_ui',
		'gro',
		'fuz_util',
		'fuz_template',
		'fuz_blog',
		'fuz_mastodon',
		'fuz_code',
		'mdz',
		'svelte-docinfo',
		'tsv',
		'tsv.fuz.dev',
		'zzz',
		'fuz_docs',
		'fuz_repos'
		// 'fuz.dev',
	]
};

export default config;
