import type {CreateGitopsConfig} from './src/lib/gitops_config.js';

const config: CreateGitopsConfig = () => {
	return {
		repos: [
			'https://github.com/fuzdev/fuz_css',
			'https://github.com/fuzdev/fuz_ui',
			'https://github.com/ryanatkn/gro',
			'https://github.com/fuzdev/fuz_util',
			'https://github.com/fuzdev/fuz_template',
			'https://github.com/fuzdev/fuz_blog',
			'https://github.com/fuzdev/fuz_mastodon',
			'https://github.com/fuzdev/fuz_code',
			{
				repo_url: 'https://github.com/fuzdev/fuz_gitops',
				branch: 'main',
			},
		],
	};
};

export default config;
