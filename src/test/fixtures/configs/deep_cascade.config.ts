import type { CreateGitopsConfig } from '$lib/gitops_config.ts';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { deep_cascade } from '../repo_fixtures/deep_cascade.ts';

// Get absolute path to fixtures directory
const FIXTURES_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

const config: CreateGitopsConfig = () => {
	const repos = [];

	// Generate repo configs from this fixture only
	for (const repo_data of deep_cascade.repos) {
		repos.push({
			repo_url: repo_data.repo_url,
			repo_dir: join(FIXTURES_DIR, 'repos', deep_cascade.name, repo_data.repo_name)
		});
	}

	return { repos };
};

export default config;
