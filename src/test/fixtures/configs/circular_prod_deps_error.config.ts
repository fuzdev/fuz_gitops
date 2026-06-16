import type {CreateGitopsConfig} from '$lib/gitops_config.ts';
import {join, dirname} from 'node:path';
import {fileURLToPath} from 'node:url';

import {circular_prod_deps_error} from '../repo_fixtures/circular_prod_deps_error.ts';

// Get absolute path to fixtures directory
const FIXTURES_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

const config: CreateGitopsConfig = () => {
	const repos = [];

	// Generate repo configs from this fixture only
	for (const repo_data of circular_prod_deps_error.repos) {
		repos.push({
			repo_url: repo_data.repo_url,
			repo_dir: join(FIXTURES_DIR, 'repos', circular_prod_deps_error.name, repo_data.repo_name),
		});
	}

	return {repos};
};

export default config;
