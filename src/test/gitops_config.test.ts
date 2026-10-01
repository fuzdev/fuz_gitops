import { assert, describe, test } from 'vitest';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
	gitops_config_leaked_private_repos,
	load_gitops_config,
	parse_gitops_config
} from '$lib/gitops_config.ts';
import { create_mock_repos_entry } from './test_helpers.ts';

const CONFIG_PATH = '/test/gitops.config.ts';

/** Parses `raw`, returning the error message it throws. */
const parse_error = (raw: unknown): string => {
	try {
		parse_gitops_config(raw, CONFIG_PATH);
	} catch (err) {
		assert(err instanceof Error);
		return err.message;
	}
	assert.fail('expected the config to be refused');
};

describe('parse_gitops_config', () => {
	test('accepts a list of registry keys, keeping their order', () => {
		const config = parse_gitops_config({ repos: ['gro', 'fuz_util', 'tsv.fuz.dev'] }, CONFIG_PATH);
		assert.deepEqual(config.repos, ['gro', 'fuz_util', 'tsv.fuz.dev']);
	});

	test('accepts an empty list (tasks refuse it before running `repos`)', () => {
		assert.deepEqual(parse_gitops_config({ repos: [] }, CONFIG_PATH).repos, []);
	});

	test('refuses a URL, naming it', () => {
		const message = parse_error({ repos: ['gro', 'https://github.com/fuzdev/fuz_ui'] });
		assert.include(message, CONFIG_PATH);
		assert.include(message, "`https://github.com/fuzdev/fuz_ui` isn't a registry key");
	});

	test('refuses an object entry, saying where its fields went', () => {
		const message = parse_error({ repos: [{ repo_url: 'https://github.com/fuzdev/gro' }] });
		assert.include(message, CONFIG_PATH);
		assert.include(message, 'come from the registry');
	});

	test('refuses a key listed twice', () => {
		assert.include(parse_error({ repos: ['gro', 'mdz', 'gro'] }), '`gro` is listed more than once');
	});

	test('refuses an empty key', () => {
		assert.include(parse_error({ repos: [''] }), CONFIG_PATH);
	});

	test('refuses unknown fields, `repos_dir` among them', () => {
		assert.include(parse_error({ repos: ['gro'], repos_dir: '..' }), 'repos_dir');
	});

	test('refuses a missing `repos`', () => {
		assert.include(parse_error({}), 'repos');
	});
});

describe('load_gitops_config', () => {
	test('a missing file is `null`', async () => {
		assert.strictEqual(await load_gitops_config('/nonexistent/gitops.config.ts'), null);
	});

	test('calls a default export in function form', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-config-'));
		try {
			const config_path = join(dir, 'gitops.config.js');
			writeFileSync(config_path, "export default async () => ({repos: ['gro', 'mdz']});\n");
			const config = await load_gitops_config(config_path);
			assert.deepEqual(config?.repos, ['gro', 'mdz']);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});
});

describe('gitops_config_leaked_private_repos', () => {
	const entries = [
		create_mock_repos_entry({ key: 'fuz_util' }),
		create_mock_repos_entry({ key: 'hidden_repo', visibility: 'private', ci: false })
	];

	test('a public host leaks its private repos', () => {
		const leaked = gitops_config_leaked_private_repos(entries, false);
		assert.deepEqual(
			leaked.map((e) => e.key),
			['hidden_repo']
		);
	});

	test('a private host leaks nothing', () => {
		assert.deepEqual(gitops_config_leaked_private_repos(entries, true), []);
	});

	test('an all-public config leaks nothing from a public host', () => {
		assert.deepEqual(gitops_config_leaked_private_repos(entries.slice(0, 1), false), []);
	});
});
