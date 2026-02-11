import type {Task} from '@fuzdev/gro';
import {styleText as st} from 'node:util';

import {generate_all_fixtures} from './generate_repos.js';
import {basic_publishing} from './repo_fixtures/basic_publishing.js';
import {deep_cascade} from './repo_fixtures/deep_cascade.js';
import {circular_dev_deps} from './repo_fixtures/circular_dev_deps.js';
import {private_packages} from './repo_fixtures/private_packages.js';
import {major_bumps} from './repo_fixtures/major_bumps.js';
import {peer_deps_only} from './repo_fixtures/peer_deps_only.js';
import {circular_prod_deps_error} from './repo_fixtures/circular_prod_deps_error.js';
import {isolated_packages} from './repo_fixtures/isolated_packages.js';
import {multiple_dep_types} from './repo_fixtures/multiple_dep_types.js';

// All fixture sets to generate
const FIXTURES = [
	basic_publishing,
	deep_cascade,
	circular_dev_deps,
	private_packages,
	major_bumps,
	peer_deps_only,
	circular_prod_deps_error,
	isolated_packages,
	multiple_dep_types,
];

/**
 * Generate all fixture git repositories from fixture data.
 * Run this before fixture tests to ensure repos exist.
 *
 * Usage:
 *   gro src/test/fixtures/generate_repos
 */
export const task: Task = {
	summary: 'generate git repositories from fixture data',
	run: async ({log}): Promise<void> => {
		log.info(st('cyan', 'Generating fixture repositories...'));

		try {
			await generate_all_fixtures(FIXTURES, log);

			log.info(st('green', '✅ All fixture repositories generated successfully'));
			log.info('   Repos are ready for testing in src/test/fixtures/repos/');
		} catch (error) {
			log.error(st('red', '❌ Failed to generate fixture repositories'));
			log.error(`   Error: ${error}`);
			throw error;
		}
	},
};
