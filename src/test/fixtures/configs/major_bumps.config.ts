import type { CreateGitopsConfig } from '$lib/gitops_config.ts';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { major_bumps } from '../repo_fixtures/major_bumps.ts';

// Get absolute path to fixtures directory
const FIXTURES_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

const config: CreateGitopsConfig = () => {
	const repos = [];

	// Generate repo configs from this fixture only
	for (const repo_data of major_bumps.repos) {
		repos.push({
			repo_url: repo_data.repo_url,
			repo_dir: join(FIXTURES_DIR, 'repos', major_bumps.name, repo_data.repo_name)
		});
	}

	return { repos };
};

export default config;
