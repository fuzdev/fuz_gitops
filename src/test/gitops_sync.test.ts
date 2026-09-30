import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { Logger } from '@fuzdev/fuz_util/log.ts';
import { TaskError } from '@fuzdev/gro';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { prepare_gitops_sync, type GitopsSyncDeps } from '$lib/gitops_sync.task.ts';
import type { ReposEntryStatus } from '$lib/repos_status.ts';
import { basic_publishing } from './fixtures/repo_fixtures/basic_publishing.ts';
import {
	create_mock_repo,
	create_mock_repos_entry,
	create_mock_repos_report
} from './test_helpers.ts';

// lists the `basic_publishing` fixture's repos by key
const CONFIG = join(
	dirname(fileURLToPath(import.meta.url)),
	'fixtures/configs/basic_publishing.config.ts'
);
const KEYS = basic_publishing.repos.map((r) => r.repo_name);

/** A logger that keeps what it logs, by stream. */
const create_capturing_log = (): Logger & { warned: Array<string>; logged: Array<string> } => {
	const warned: Array<string> = [];
	const logged: Array<string> = [];
	const log = new Logger('test', {
		level: 'info',
		colors: false,
		console: {
			log: (...args) => logged.push(args.join(' ')),
			warn: (...args) => warned.push(args.join(' ')),
			error: (...args) => logged.push(args.join(' '))
		}
	});
	return Object.assign(log, { warned, logged });
};

const primary = (key: string) => create_mock_repos_entry({ key }).checkouts[0]!;

const OFF_BRANCH = (key: string): ReposEntryStatus =>
	create_mock_repos_entry({
		key,
		checkouts: [{ ...primary(key), head: { kind: 'branch', name: 'feature' } }],
		at_rest: { on_branch: false, clean: true, idle: true, followed: { kind: 'in_sync' } }
	});

const DIRTY = (key: string): ReposEntryStatus =>
	create_mock_repos_entry({
		key,
		checkouts: [
			{ ...primary(key), uncommitted: { staged: 0, unstaged: 2, untracked: 0, conflicted: 0 } }
		],
		at_rest: { on_branch: true, clean: false, idle: true, followed: { kind: 'in_sync' } }
	});

const REBASING = (key: string): ReposEntryStatus =>
	create_mock_repos_entry({
		key,
		checkouts: [{ ...primary(key), in_progress: 'rebase' }],
		at_rest: { on_branch: true, clean: true, idle: false, followed: { kind: 'in_sync' } }
	});

/**
 * Deps recording each step in `steps`: each `repos status` run (with
 * `--fetch` or not), the token read, and the library load. Every repo dir
 * holds a `package.json` and `node_modules` unless `missing` names the path.
 */
const create_recording_deps = (
	options: {
		/** Entries the local `repos status` reports, by key; the rest are ready. */
		local?: Record<string, ReposEntryStatus>;
		/** Entries the fetched `repos status` reports, by key; defaults to `local`'s. */
		fetched?: Record<string, ReposEntryStatus>;
		token?: string;
		missing?: Array<string>;
		files?: Record<string, string>;
	} = {}
): { deps: GitopsSyncDeps; steps: Array<string> } => {
	const { local = {}, fetched = local, token = 'token', missing = [], files = {} } = options;
	const steps: Array<string> = [];
	return {
		steps,
		deps: {
			repos_ops: {
				status: async ({ keys, fetch }) => {
					steps.push(`repos status${fetch ? ' --fetch' : ''}`);
					const by_key = fetch ? fetched : local;
					const report = create_mock_repos_report(
						keys.map((key) => by_key[key] ?? create_mock_repos_entry({ key })),
						{ fetched: fetch === true }
					);
					return {
						ok: true,
						output: { stdout: JSON.stringify(report), stderr: '', exit_code: 0 }
					};
				}
			},
			load_repos: async ({ local_repo_paths }) => {
				steps.push('load');
				return local_repo_paths.map((p) => {
					const repo = create_mock_repo({ name: p.repo_name });
					repo.entry = p.entry;
					return repo;
				});
			},
			load_token: () => {
				steps.push('token');
				return token || undefined;
			},
			// every repo has a `package.json` and `node_modules`; other paths exist only in `files`
			exists: (path) =>
				!missing.includes(path) &&
				(path in files || path.endsWith('/package.json') || path.endsWith('/node_modules')),
			read_file: (path) => files[path] ?? ''
		}
	};
};

describe('prepare_gitops_sync', () => {
	test('a ready set fetches, then loads', async () => {
		const fetched = Object.fromEntries(
			KEYS.map((key) => [key, create_mock_repos_entry({ key, fetched_at: 1234 })])
		);
		const { deps, steps } = create_recording_deps({ fetched });
		const log = create_capturing_log();
		const prepared = await prepare_gitops_sync({ config: CONFIG, log }, deps);
		assert.ok(prepared);
		assert.strictEqual(prepared.token, 'token');
		assert.deepEqual(
			prepared.local_repos.map((r) => r.entry.key),
			KEYS
		);
		// the loaded repos carry the fetched report's entries
		assert.ok(prepared.local_repos.every((r) => r.entry.fetched_at === 1234));
		assert.deepEqual(steps, ['repos status', 'token', 'repos status --fetch', 'load']);
		assert.deepEqual(log.warned, []);
	});

	test('refuses a repo off its branch, dirty, or mid-operation before any network, naming each', async () => {
		const { deps, steps } = create_recording_deps({
			local: { repo_a: OFF_BRANCH('repo_a'), repo_b: DIRTY('repo_b'), repo_c: REBASING('repo_c') }
		});
		const err = await assert_rejects(() =>
			prepare_gitops_sync({ config: CONFIG, log: create_capturing_log() }, deps)
		);
		assert.ok(err instanceof TaskError);
		assert.include(err.message, 'repo_a: on `feature`, not `main` — switch to `main`');
		assert.include(err.message, 'repo_b: uncommitted changes (2 unstaged)');
		assert.include(err.message, 'repo_c: a rebase is in progress — finish or abort it');
		assert.include(err.message, '`--allow_dirty`');
		assert.notInclude(err.message, 'repo_d');
		// no token read, no fetch, no load
		assert.deepEqual(steps, ['repos status']);
	});

	test('refuses what the fetched report finds not ready', async () => {
		const { deps, steps } = create_recording_deps({ fetched: { repo_d: DIRTY('repo_d') } });
		await assert_rejects(
			() => prepare_gitops_sync({ config: CONFIG, log: create_capturing_log() }, deps),
			/repo_d: uncommitted changes/
		);
		assert.deepEqual(steps, ['repos status', 'token', 'repos status --fetch']);
	});

	test('--allow_dirty proceeds, warning on each', async () => {
		const local = {
			repo_a: OFF_BRANCH('repo_a'),
			repo_b: DIRTY('repo_b'),
			repo_c: REBASING('repo_c')
		};
		const { deps, steps } = create_recording_deps({ local });
		const log = create_capturing_log();
		const prepared = await prepare_gitops_sync({ config: CONFIG, allow_dirty: true, log }, deps);
		assert.ok(prepared);
		assert.deepEqual(steps, ['repos status', 'token', 'repos status --fetch', 'load']);
		const warned = log.warned.join('\n');
		assert.include(warned, 'repo_a: on `feature`, not `main`');
		assert.include(warned, 'repo_b: uncommitted changes (2 unstaged)');
		assert.include(warned, 'repo_c: a rebase is in progress');
	});

	test('behind origin warns', async () => {
		const behind = create_mock_repos_entry({
			key: 'repo_e',
			at_rest: {
				on_branch: true,
				clean: true,
				idle: true,
				followed: { kind: 'behind', commits: 3 }
			}
		});
		const { deps } = create_recording_deps({ fetched: { repo_e: behind } });
		const log = create_capturing_log();
		assert.ok(await prepare_gitops_sync({ config: CONFIG, log }, deps));
		assert.include(
			log.warned.join('\n'),
			'repo_e: `main` is 3 commits behind origin (never fetched) — `repos sync repo_e` fast-forwards it'
		);
	});

	test('a failed fetch warns', async () => {
		const failed = create_mock_repos_entry({
			key: 'repo_b',
			fetch_error: { kind: 'timed_out', after_secs: 60 }
		});
		const { deps } = create_recording_deps({ fetched: { repo_b: failed } });
		const log = create_capturing_log();
		assert.ok(await prepare_gitops_sync({ config: CONFIG, log }, deps));
		assert.include(log.warned.join('\n'), 'repo_b: fetching origin failed (timed out after 60s)');
	});

	test('a missing token fails before fetching', async () => {
		const { deps, steps } = create_recording_deps({ token: '' });
		await assert_rejects(
			() => prepare_gitops_sync({ config: CONFIG, log: create_capturing_log() }, deps),
			/SECRET_GITHUB_API_TOKEN/
		);
		assert.deepEqual(steps, ['repos status', 'token']);
	});

	test('warns on each npm repo the analysis can not fully read', async () => {
		const { deps } = create_recording_deps({
			missing: ['/test/repo_a/node_modules', '/test/repo_b/.svelte-kit/tsconfig.json'],
			files: {
				'/test/repo_b/tsconfig.json': '{"extends": "./.svelte-kit/tsconfig.json"}',
				'/test/repo_c/tsconfig.json': '{"extends": "./.svelte-kit/tsconfig.json"}',
				'/test/repo_c/.svelte-kit/tsconfig.json': '{}',
				'/test/repo_d/tsconfig.json': '{"compilerOptions": {}}'
			}
		});
		const log = create_capturing_log();
		assert.ok(await prepare_gitops_sync({ config: CONFIG, log }, deps));
		const gaps = log.warned
			.filter((w) => w.includes(': no '))
			.map((w) => w.replace(/^.*\[test\] /, ''));
		assert.deepEqual(gaps, [
			'  repo_a: no `node_modules`',
			'  repo_b: no `.svelte-kit/tsconfig.json`'
		]);
	});

	test('a repo without a package.json is not warned about', async () => {
		const { deps } = create_recording_deps({
			missing: ['/test/repo_a/package.json', '/test/repo_a/node_modules']
		});
		const log = create_capturing_log();
		assert.ok(await prepare_gitops_sync({ config: CONFIG, log }, deps));
		assert.deepEqual(log.warned, []);
	});
});

describe('prepare_gitops_sync --check', () => {
	test('reports from local refs alone: no token, no fetch, no load', async () => {
		const behind = create_mock_repos_entry({
			key: 'repo_e',
			at_rest: {
				on_branch: true,
				clean: true,
				idle: true,
				followed: { kind: 'behind', commits: 1 }
			}
		});
		const { deps, steps } = create_recording_deps({ local: { repo_e: behind }, token: '' });
		const log = create_capturing_log();
		const prepared = await prepare_gitops_sync({ config: CONFIG, check: true, log }, deps);
		assert.strictEqual(prepared, null);
		assert.deepEqual(steps, ['repos status']);
		assert.include(log.warned.join('\n'), 'repo_e: `main` is 1 commit behind origin');
		assert.include(log.logged.join('\n'), 'ready to generate');
	});

	test('fails naming what a real run would refuse', async () => {
		const { deps, steps } = create_recording_deps({
			local: { repo_a: OFF_BRANCH('repo_a'), repo_b: DIRTY('repo_b') },
			token: ''
		});
		const err = await assert_rejects(() =>
			prepare_gitops_sync({ config: CONFIG, check: true, log: create_capturing_log() }, deps)
		);
		assert.ok(err instanceof TaskError);
		assert.include(err.message, 'repo_a: on `feature`');
		assert.include(err.message, 'repo_b: uncommitted changes');
		assert.deepEqual(steps, ['repos status']);
	});

	test('with --allow_dirty passes what a real run with it would, warning', async () => {
		const { deps, steps } = create_recording_deps({
			local: { repo_a: OFF_BRANCH('repo_a') },
			token: ''
		});
		const log = create_capturing_log();
		const prepared = await prepare_gitops_sync(
			{ config: CONFIG, check: true, allow_dirty: true, log },
			deps
		);
		assert.strictEqual(prepared, null);
		assert.deepEqual(steps, ['repos status']);
		assert.include(log.warned.join('\n'), 'repo_a: on `feature`, not `main`');
	});

	test('keeps the public-host guard', async () => {
		const { deps, steps } = create_recording_deps({
			local: { repo_c: create_mock_repos_entry({ key: 'repo_c', visibility: 'private' }) }
		});
		await assert_rejects(
			() =>
				prepare_gitops_sync(
					{
						config: CONFIG,
						check: true,
						host: { name: '@test/host', private: false },
						log: create_capturing_log()
					},
					deps
				),
			/`repo_c` is private, and @test\/host is a public package/
		);
		assert.deepEqual(steps, ['repos status']);
	});
});
