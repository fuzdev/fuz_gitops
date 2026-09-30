import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { TaskError } from '@fuzdev/gro';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { resolve_gitops_repos } from '$lib/gitops_task_helpers.ts';
import { basic_publishing } from './fixtures/repo_fixtures/basic_publishing.ts';
import {
	create_mock_repos_entry,
	create_mock_repos_ops,
	create_mock_repos_report
} from './test_helpers.ts';

const TEST_DIR = dirname(fileURLToPath(import.meta.url));

// lists the `basic_publishing` fixture's repos by key
const CONFIG = join(TEST_DIR, 'fixtures/configs/basic_publishing.config.ts');
const KEYS = basic_publishing.repos.map((r) => r.repo_name);

/** A report on `KEYS`, in reverse, as `repos` needn't keep config order. */
const create_report = (
	overrides: Record<string, Parameters<typeof create_mock_repos_entry>[0]> = {}
) =>
	create_mock_repos_report(
		[...KEYS].reverse().map((key) => create_mock_repos_entry(overrides[key] ?? { key }))
	);

describe('resolve_gitops_repos', () => {
	test('runs `repos status` on the config keys and resolves them in config order', async () => {
		const repos_ops = create_mock_repos_ops(create_report());
		const { gitops_config, local_repo_paths } = await resolve_gitops_repos({
			config: CONFIG,
			registry: '/elsewhere/repos.toml',
			repos_ops
		});
		assert.deepEqual(gitops_config.repos, KEYS);
		assert.deepEqual(repos_ops.calls, [{ keys: KEYS, registry: '/elsewhere/repos.toml' }]);
		assert.deepEqual(
			local_repo_paths.map((p) => p.repo_name),
			KEYS
		);
		assert.deepEqual(
			local_repo_paths.map((p) => p.repo_dir),
			KEYS.map((k) => `/test/${k}`)
		);
	});

	test('an error document fails with its message', async () => {
		const error_doc = JSON.parse(
			readFileSync(join(TEST_DIR, 'fixtures/repos_status/error_report_unknown_entry.json'), 'utf8')
		);
		await assert_rejects(
			() => resolve_gitops_repos({ config: CONFIG, repos_ops: create_mock_repos_ops(error_doc) }),
			/basic_publishing\.config\.ts: the gitops config lists `grimoir`, which the registry \(repos\.toml\) doesn't name — did you mean `grimoire`\?/
		);
	});

	test('a missing repo fails the load, naming it and the config', async () => {
		const repos_ops = create_mock_repos_ops(
			create_report({
				repo_c: {
					key: 'repo_c',
					presence: { kind: 'missing' },
					checkouts: [],
					at_rest: null,
					layout: null
				}
			})
		);
		const err = await assert_rejects(
			() => resolve_gitops_repos({ config: CONFIG, repos_ops }),
			/`repo_c` is missing at \/test\/repo_c — `repos sync repo_c` clones it/
		);
		assert.include(err.message, 'basic_publishing.config.ts');
	});

	test('the missing-repo hint repeats `--registry`', async () => {
		const repos_ops = create_mock_repos_ops(
			create_report({
				repo_c: {
					key: 'repo_c',
					presence: { kind: 'missing' },
					checkouts: [],
					at_rest: null,
					layout: null
				}
			})
		);
		await assert_rejects(
			() => resolve_gitops_repos({ config: CONFIG, registry: '../repos.toml', repos_ops }),
			/`repos --registry \.\.\/repos\.toml sync repo_c` clones it/
		);
	});

	test('a config that throws fails as a TaskError carrying its location', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-helpers-'));
		try {
			const config = join(dir, 'gitops.config.js');
			writeFileSync(config, "export default () => { throw new TypeError('boom'); };\n");
			const repos_ops = create_mock_repos_ops(create_report());
			const err = await assert_rejects(() => resolve_gitops_repos({ config, repos_ops }));
			assert.ok(err instanceof TaskError);
			assert.include(err.message, `The gitops config at ${config} threw`);
			assert.include(err.message, 'boom');
			assert.include(err.message, 'gitops.config.js:1');
			assert.deepEqual(repos_ops.calls, []);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});

	test('an old URL config fails as a TaskError with the migration message', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-helpers-'));
		try {
			const config = join(dir, 'gitops.config.js');
			writeFileSync(
				config,
				"export default () => ({repos: ['https://github.com/fuzdev/gro', {repo_url: 'https://github.com/fuzdev/fuz_repos', repo_dir: '../fuz_gitops'}]});\n"
			);
			const repos_ops = create_mock_repos_ops(create_report());
			const err = await assert_rejects(() => resolve_gitops_repos({ config, repos_ops }));
			assert.ok(
				err instanceof TaskError,
				'an invalid config is a TaskError, not an unexpected error'
			);
			assert.include(err.message, config);
			assert.include(err.message, 'come from the registry');
			assert.deepEqual(repos_ops.calls, []);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});

	test('a public host refuses a private repo', async () => {
		const repos_ops = create_mock_repos_ops(
			create_report({ repo_b: { key: 'repo_b', visibility: 'private', ci: false } })
		);
		await assert_rejects(
			() =>
				resolve_gitops_repos({
					config: CONFIG,
					host: { name: '@test/site', private: false },
					repos_ops
				}),
			/`repo_b` is private, and @test\/site is a public package/
		);
	});

	test('a config listing no repos fails before running `repos`', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-helpers-'));
		try {
			const config = join(dir, 'gitops.config.js');
			writeFileSync(config, 'export default {repos: []};\n');
			const repos_ops = create_mock_repos_ops(create_report());
			await assert_rejects(
				() => resolve_gitops_repos({ config, repos_ops }),
				/No repos are configured/
			);
			assert.deepEqual(repos_ops.calls, []);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});

	test('a missing config fails', async () => {
		await assert_rejects(
			() =>
				resolve_gitops_repos({
					config: '/nonexistent/gitops.config.ts',
					repos_ops: create_mock_repos_ops(create_report())
				}),
			/No gitops config found/
		);
	});
});
