import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { Logger } from '@fuzdev/fuz_util/log.ts';
import { TaskError } from '@fuzdev/gro';

import { Args, run_gitops_publish, type GitopsPublishDeps } from '$lib/gitops_publish.task.ts';
import { gate_publish_readiness } from '$lib/gitops_task_helpers.ts';
import type { LocalRepo } from '$lib/local_repo.ts';
import type { ReposEntryStatus } from '$lib/repos_status.ts';
import {
	create_mock_gitops_ops,
	create_mock_repo,
	create_mock_repos_entry,
	create_mock_repos_ops,
	create_mock_repos_report,
	create_populated_fs_ops
} from './test_helpers.ts';

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

const OFF_BRANCH: Partial<ReposEntryStatus> = {
	checkouts: [
		{
			...create_mock_repos_entry({ key: 'b' }).checkouts[0]!,
			head: { kind: 'branch', name: 'feature' }
		}
	],
	at_rest: { on_branch: false, clean: true, idle: true, followed: { kind: 'in_sync' } }
};

/** Two npm repos and a cargo one; `b`'s entry as the local, unfetched status read it. */
const create_repos = (b_entry: Partial<ReposEntryStatus> = {}): Array<LocalRepo> => {
	const a = create_mock_repo({ name: 'a' });
	const b = create_mock_repo({ name: 'b', deps: { a: '^1.0.0' } });
	b.entry = create_mock_repos_entry({ key: 'b', ...b_entry });
	const c = create_mock_repo({ name: 'c', kind: 'cargo' });
	return [a, b, c];
};

/**
 * Deps recording each step in `steps`, in order: the load, each `repos status`
 * run (with `--fetch` or not), the prompt, preflight, each executor re-check,
 * and every spawn and commit.
 */
const create_recording_deps = (options: {
	repos: Array<LocalRepo>;
	/** The entries the gate's fetched `repos status` reports. */
	fetched_entries: Array<ReposEntryStatus>;
	confirm?: boolean;
}): { deps: GitopsPublishDeps; steps: Array<string> } => {
	const { repos, fetched_entries, confirm = true } = options;
	const steps: Array<string> = [];
	const inner = create_mock_repos_ops(create_mock_repos_report(fetched_entries, { fetched: true }));
	const ops = create_mock_gitops_ops({ fs: create_populated_fs_ops(repos) });
	return {
		steps,
		deps: {
			load_repos: async () => {
				steps.push('load');
				return { local_repos: repos };
			},
			repos_ops: {
				status: async (status_options) => {
					steps.push(
						`repos status ${status_options.keys.join(' ')}${status_options.fetch ? ' --fetch' : ''}`
					);
					return inner.status(status_options);
				}
			},
			ops: {
				...ops,
				preflight: {
					run_preflight_checks: async (preflight_options) => {
						steps.push('preflight');
						return create_mock_gitops_ops().preflight.run_preflight_checks(preflight_options);
					}
				},
				process: {
					spawn: async ({ cmd, args }) => {
						steps.push(`spawn ${cmd} ${args.join(' ')}`);
						return { ok: true };
					}
				},
				git: {
					...ops.git,
					commit: async () => {
						steps.push('commit');
						return { ok: true };
					}
				},
				repos: {
					status: async (status_options) => {
						steps.push(`recheck ${status_options.keys.join(' ')}`);
						return ops.repos.status(status_options);
					}
				}
			},
			confirm: async () => {
				steps.push('confirm');
				return confirm;
			}
		}
	};
};

describe('run_gitops_publish --wetrun', () => {
	test('the readiness gate refuses before the prompt and any side effect', async () => {
		const repos = create_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({ key: 'b', ...OFF_BRANCH })
			]
		});
		const err = await assert_rejects(() =>
			run_gitops_publish(Args.parse({ wetrun: true }), create_capturing_log(), deps)
		);
		assert.ok(err instanceof TaskError);
		assert.include(err.message, 'b: on `feature`, not `main`');
		assert.include(err.message, 'nothing was changed');
		// fetched the npm repos alone, then stopped: no prompt, preflight, spawn, or commit
		assert.deepEqual(steps, ['load', 'repos status a b --fetch']);
	});

	test('a ready set passes the gate, then prompts, then publishes', async () => {
		const repos = create_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({ key: 'b' })
			]
		});
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true }),
			create_capturing_log(),
			deps
		);
		assert.strictEqual(outcome, 'done');
		assert.deepEqual(steps.slice(0, 6), [
			'load',
			'repos status a b --fetch',
			'confirm',
			'preflight',
			'recheck a',
			'spawn gro publish --no-build --no-pull --branch main'
		]);
	});

	test('the gate passes a repo ahead of origin and says its release push carries it', async () => {
		const repos = create_repos();
		const { deps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({
					key: 'b',
					at_rest: {
						on_branch: true,
						clean: true,
						idle: true,
						followed: { kind: 'ahead', commits: 2 }
					}
				})
			],
			confirm: false
		});
		const log = create_capturing_log();
		const outcome = await run_gitops_publish(Args.parse({ wetrun: true }), log, deps);
		assert.strictEqual(outcome, 'cancelled');
		assert.ok(
			log.logged.some((l) =>
				l.includes(
					'b: `main` is 2 commits ahead of origin — publishing pushes them with the release'
				)
			)
		);
	});

	test('a plan with errors fails before fetching, with or without --no-plan', async () => {
		// a production cycle: a ↔ b
		const a = create_mock_repo({ name: 'a', deps: { b: '^1.0.0' } });
		const b = create_mock_repo({ name: 'b', deps: { a: '^1.0.0' } });
		const shown = create_recording_deps({ repos: [a, b], fetched_entries: [] });
		await assert_rejects(() =>
			run_gitops_publish(Args.parse({ wetrun: true }), create_capturing_log(), shown.deps)
		);
		assert.deepEqual(shown.steps, ['load']);

		const unshown = create_recording_deps({ repos: [a, b], fetched_entries: [] });
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true, plan: false }),
			create_capturing_log(),
			unshown.deps
		);
		assert.strictEqual(outcome, 'failed');
		assert.deepEqual(unshown.steps, ['load']);
	});

	test('declining the prompt changes nothing', async () => {
		const repos = create_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({ key: 'b' })
			],
			confirm: false
		});
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true }),
			create_capturing_log(),
			deps
		);
		assert.strictEqual(outcome, 'cancelled');
		assert.deepEqual(steps, ['load', 'repos status a b --fetch', 'confirm']);
	});

	test('--no-plan skips the prompt but not the gate', async () => {
		const repos = create_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a', fetch_error: { kind: 'timed_out', after_secs: 60 } }),
				create_mock_repos_entry({ key: 'b' })
			]
		});
		await assert_rejects(
			() =>
				run_gitops_publish(Args.parse({ wetrun: true, plan: false }), create_capturing_log(), deps),
			/a: fetching origin failed \(timed out after 60s\)/
		);
		assert.deepEqual(steps, ['load', 'repos status a b --fetch']);
	});
});

describe('run_gitops_publish dry run', () => {
	test('runs no gate and prints the readiness block', async () => {
		const repos = create_repos(OFF_BRANCH);
		const { deps, steps } = create_recording_deps({ repos, fetched_entries: [] });
		const log = create_capturing_log();
		const outcome = await run_gitops_publish(Args.parse({}), log, deps);
		assert.strictEqual(outcome, 'done');
		assert.deepEqual(steps, ['load']);
		const block = log.warned.join('\n');
		assert.include(block, 'not at rest, so read as they sit');
		assert.include(block, 'b: on `feature`, not `main`');
	});

	test('prints no block when every repo is at rest', async () => {
		const repos = create_repos();
		const { deps } = create_recording_deps({ repos, fetched_entries: [] });
		const log = create_capturing_log();
		await run_gitops_publish(Args.parse({}), log, deps);
		assert.notInclude(log.warned.join('\n'), 'not at rest');
	});
});

describe('gate_publish_readiness', () => {
	test('fetches the npm repos alone, passing `--registry`', async () => {
		const repos_ops = create_mock_repos_ops(
			create_mock_repos_report(
				[create_mock_repos_entry({ key: 'a' }), create_mock_repos_entry({ key: 'b' })],
				{ fetched: true }
			)
		);
		await gate_publish_readiness({ local_repos: create_repos(), registry: '../r.toml', repos_ops });
		assert.deepEqual(repos_ops.calls, [{ keys: ['a', 'b'], registry: '../r.toml', fetch: true }]);
	});

	test('fixes in a refusal name `--registry`', async () => {
		const repos_ops = create_mock_repos_ops(
			create_mock_repos_report(
				[
					create_mock_repos_entry({ key: 'a' }),
					create_mock_repos_entry({
						key: 'b',
						at_rest: {
							on_branch: true,
							clean: true,
							idle: true,
							followed: { kind: 'behind', commits: 1 }
						}
					})
				],
				{ fetched: true }
			)
		);
		await assert_rejects(
			() =>
				gate_publish_readiness({ local_repos: create_repos(), registry: '../r.toml', repos_ops }),
			/`repos --registry \.\.\/r\.toml sync b` fast-forwards it/
		);
	});

	test('runs nothing with no npm repos', async () => {
		const repos_ops = create_mock_repos_ops('unused');
		await gate_publish_readiness({
			local_repos: [create_mock_repo({ name: 'c', kind: 'cargo' })],
			repos_ops
		});
		assert.deepEqual(repos_ops.calls, []);
	});

	test('a failed `repos status` refuses', async () => {
		const repos_ops = create_mock_repos_ops('not json');
		await assert_rejects(
			() => gate_publish_readiness({ local_repos: create_repos(), repos_ops }),
			/the readiness check failed/
		);
	});
});
