import type {CreateGitopsConfig} from '$lib/gitops_config.ts';
import {join, dirname} from 'node:path';
import {fileURLToPath} from 'node:url';

import {peer_deps_only} from '../repo_fixtures/peer_deps_only.ts';

// Get absolute path to fixtures directory
const FIXTURES_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

const config: CreateGitopsConfig = () => {
	const repos = [];

	// Generate repo configs from this fixture only
	for (const repo_data of peer_deps_only.repos) {
		repos.push({
			repo_url: repo_data.repo_url,
			repo_dir: join(FIXTURES_DIR, 'repos', peer_deps_only.name, repo_data.repo_name),
		});
	}

	return {repos};
};

export default config;
