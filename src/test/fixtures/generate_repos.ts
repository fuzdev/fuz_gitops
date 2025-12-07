import {mkdirSync, rmSync, existsSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {spawn_out} from '@fuzdev/fuz_util/process.js';
import type {Logger} from '@fuzdev/fuz_util/log.js';

import type {RepoFixtureSet, RepoFixtureData} from './repo_fixture_types.js';

const FIXTURES_BASE_DIR = 'src/test/fixtures/repos';

/**
 * Generate all repositories for multiple fixture sets.
 */
export const generate_all_fixtures = async (
	fixtures: Array<RepoFixtureSet>,
	log?: Logger,
): Promise<void> => {
	log?.info(`Generating ${fixtures.length} fixture sets...\n`);

	for (const fixture of fixtures) {
		await generate_fixture_set(fixture, log); // eslint-disable-line no-await-in-loop
		log?.info(''); // blank line between fixtures
	}

	log?.info(`✅ Generated all fixtures in ${FIXTURES_BASE_DIR}`);
};

/**
 * Check if fixture repos have been generated.
 */
export const fixtures_exist = (fixture_name: string): boolean =>
	existsSync(join(FIXTURES_BASE_DIR, fixture_name));

/**
 * Generate a single git repository from fixture data.
 * Creates directory structure, package.json, changesets, and initializes git.
 */
const generate_fixture_repo = async (
	repo_data: RepoFixtureData,
	fixture_name: string,
	log?: Logger,
): Promise<string> => {
	// Each fixture has its own workspace directory with repos inside
	const repo_path = join(FIXTURES_BASE_DIR, fixture_name, repo_data.repo_name);

	log?.info(`  Generating ${repo_data.repo_name}...`);

	// Create repo directory
	mkdirSync(repo_path, {recursive: true});

	// Write package.json with repository field (standard npm format)
	const package_json_path = join(repo_path, 'package.json');
	const package_json_with_repository = {
		...repo_data.package_json,
		repository: repo_data.repo_url, // Use standard npm field, gro derives repo_url from this
		type: 'module', // Required for ESM
	};
	writeFileSync(package_json_path, JSON.stringify(package_json_with_repository, null, '\t') + '\n');

	// Create minimal svelte.config.js
	const svelte_config_content = `/** @type {import('@sveltejs/kit').Config} */
const config = {
	kit: {}
};

export default config;
`;
	writeFileSync(join(repo_path, 'svelte.config.js'), svelte_config_content);

	// Create src/lib directory so gro can find source files
	mkdirSync(join(repo_path, 'src/lib'), {recursive: true});
	// Create empty index.ts so the lib directory isn't empty
	writeFileSync(
		join(repo_path, 'src/lib/index.ts'),
		'export const value = "Fixture repo module";\n',
	);

	// Create .changeset directory and files if changesets exist
	if (repo_data.changesets && repo_data.changesets.length > 0) {
		const changeset_dir = join(repo_path, '.changeset');
		mkdirSync(changeset_dir, {recursive: true});

		// Write config.json for changesets
		const changeset_config = {
			$schema: 'https://unpkg.com/@changesets/config@3.0.3/schema.json',
			changelog: '@changesets/cli/changelog',
			commit: false,
			fixed: [],
			linked: [],
			access: 'public',
			baseBranch: 'main',
			updateInternalDependencies: 'patch',
			ignore: [],
		};
		writeFileSync(
			join(changeset_dir, 'config.json'),
			JSON.stringify(changeset_config, null, '\t') + '\n',
		);

		// Write changeset files
		for (const changeset of repo_data.changesets) {
			writeFileSync(join(changeset_dir, changeset.filename), changeset.content);
		}
	}

	// Initialize git repo with main as initial branch
	await spawn_out('git', ['init', '-b', 'main'], {cwd: repo_path});
	await spawn_out('git', ['config', 'user.email', 'test@example.com'], {cwd: repo_path});
	await spawn_out('git', ['config', 'user.name', 'Test User'], {cwd: repo_path});

	// Initial commit
	await spawn_out('git', ['add', '.'], {cwd: repo_path});
	await spawn_out('git', ['commit', '-m', 'Initial commit from fixture data'], {cwd: repo_path});

	log?.info(`    ✓ Created ${repo_path}`);

	return repo_path;
};

/**
 * Generate all repositories for a fixture set.
 * Idempotent: removes existing fixture directory before generating.
 */
const generate_fixture_set = async (fixture: RepoFixtureSet, log?: Logger): Promise<void> => {
	const fixture_dir = join(FIXTURES_BASE_DIR, fixture.name);

	log?.info(`Generating fixture set: ${fixture.name}`);

	// Remove existing fixture directory to ensure clean state
	if (existsSync(fixture_dir)) {
		log?.info(`  Removing existing directory: ${fixture_dir}`);
		rmSync(fixture_dir, {recursive: true, force: true});
	}

	// Create base directory
	mkdirSync(fixture_dir, {recursive: true});

	// Generate each repo
	for (const repo_data of fixture.repos) {
		await generate_fixture_repo(repo_data, fixture.name, log); // eslint-disable-line no-await-in-loop
	}

	log?.info(`✓ Generated ${fixture.repos.length} repos for ${fixture.name}`);
};
