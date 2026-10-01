import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { TaskError } from '@fuzdev/gro';

import {
	Args,
	format_failure_markdown,
	run_gitops_publish,
	to_child_stdout,
	type GitopsPublishDeps
} from '$lib/gitops_publish.task.ts';
import type { LocalRepo } from '$lib/local_repo.ts';
import type { ReposEntryStatus } from '$lib/repos_status.ts';
import {
	AT_REST,
	create_gate_repos,
	create_mock_gitops_ops,
	create_mock_repo,
	create_mock_repos_entry,
	create_mock_repos_ops,
	create_mock_repos_report,
	create_populated_fs_ops,
	create_stream_log,
	entry_with
} from './test_helpers.ts';

// `b` on a feature branch
const OFF_BRANCH = entry_with('b', {
	checkout: { head: { kind: 'branch', name: 'feature' } },
	at_rest: { ...AT_REST, on_branch: false }
});

/**
 * Deps recording each step in `steps`, in order: the load, each `repos status`
 * run (with `--fetch` or not), the prompt, preflight, each executor re-check,
 * and every command run and commit.
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
					run_interactive: async ({ cmd, args }) => {
						steps.push(`run ${cmd} ${args.join(' ')}`);
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
		const repos = create_gate_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [create_mock_repos_entry({ key: 'a' }), OFF_BRANCH]
		});
		const err = await assert_rejects(() =>
			run_gitops_publish(Args.parse({ wetrun: true }), create_stream_log(), deps)
		);
		assert.ok(err instanceof TaskError);
		assert.include(err.message, 'b: on `feature`, not `main`');
		assert.include(err.message, 'nothing was changed');
		// fetched the npm repos alone, then stopped: no prompt, preflight, command, or commit
		assert.deepEqual(steps, ['load', 'repos status a b --fetch']);
	});

	test('a ready set passes the gate, then prompts, then publishes', async () => {
		const repos = create_gate_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({ key: 'b' })
			]
		});
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true }),
			create_stream_log(),
			deps
		);
		assert.strictEqual(outcome, 'done');
		assert.deepEqual(steps.slice(0, 6), [
			'load',
			'repos status a b --fetch',
			'confirm',
			'preflight',
			'recheck a',
			'run gro publish --no-build --no-pull --branch main'
		]);
	});

	test('the gate passes a repo ahead of origin and says its release push carries it', async () => {
		const repos = create_gate_repos();
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
		const log = create_stream_log();
		const outcome = await run_gitops_publish(Args.parse({ wetrun: true }), log, deps);
		assert.strictEqual(outcome, 'cancelled');
		assert.ok(
			log.stdout.some((l) =>
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
			run_gitops_publish(Args.parse({ wetrun: true }), create_stream_log(), shown.deps)
		);
		assert.deepEqual(shown.steps, ['load']);

		const unshown = create_recording_deps({ repos: [a, b], fetched_entries: [] });
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true, plan: false }),
			create_stream_log(),
			unshown.deps
		);
		assert.strictEqual(outcome, 'failed');
		assert.deepEqual(unshown.steps, ['load']);
	});

	test('declining the prompt changes nothing', async () => {
		const repos = create_gate_repos();
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
			create_stream_log(),
			deps
		);
		assert.strictEqual(outcome, 'cancelled');
		assert.deepEqual(steps, ['load', 'repos status a b --fetch', 'confirm']);
	});

	test('--no-plan skips the prompt but not the gate', async () => {
		const repos = create_gate_repos();
		const { deps, steps } = create_recording_deps({
			repos,
			fetched_entries: [
				create_mock_repos_entry({ key: 'a', fetch_error: { kind: 'timed_out', after_secs: 60 } }),
				create_mock_repos_entry({ key: 'b' })
			]
		});
		await assert_rejects(
			() =>
				run_gitops_publish(Args.parse({ wetrun: true, plan: false }), create_stream_log(), deps),
			/a: fetching origin failed \(timed out after 60s\)/
		);
		assert.deepEqual(steps, ['load', 'repos status a b --fetch']);
	});
});

describe('run_gitops_publish dry run', () => {
	test('runs no gate and prints the readiness block', async () => {
		const repos = create_gate_repos(OFF_BRANCH);
		const { deps, steps } = create_recording_deps({ repos, fetched_entries: [] });
		const log = create_stream_log();
		const outcome = await run_gitops_publish(Args.parse({}), log, deps);
		assert.strictEqual(outcome, 'done');
		assert.deepEqual(steps, ['load']);
		const block = log.stderr.join('\n');
		assert.include(block, 'not at rest, so read as they sit');
		assert.include(block, 'b: on `feature`, not `main`');
	});

	test('prints no block when every repo is at rest', async () => {
		const repos = create_gate_repos();
		const { deps } = create_recording_deps({ repos, fetched_entries: [] });
		const log = create_stream_log();
		await run_gitops_publish(Args.parse({}), log, deps);
		assert.notInclude(log.stderr.join('\n'), 'not at rest');
	});
});

describe('run_gitops_publish machine output', () => {
	const ready_entries = () => [
		create_mock_repos_entry({ key: 'a' }),
		create_mock_repos_entry({ key: 'b' })
	];

	test('--format json dry run: stdout carries the report alone', async () => {
		const { deps } = create_recording_deps({
			repos: create_gate_repos(OFF_BRANCH),
			fetched_entries: []
		});
		const log = create_stream_log();
		const outcome = await run_gitops_publish(Args.parse({ format: 'json' }), log, deps);
		assert.strictEqual(outcome, 'done');
		assert.strictEqual(log.stdout.length, 1);
		const report = JSON.parse(log.stdout[0]!);
		assert.strictEqual(report.ok, true);
		// the readiness block and the executor's progress went to stderr
		assert.include(log.stderr.join('\n'), 'b: on `feature`, not `main`');
	});

	test('--format markdown --wetrun: the plan, the gate, and the progress go to stderr', async () => {
		const { deps, steps } = create_recording_deps({
			repos: create_gate_repos(),
			fetched_entries: ready_entries()
		});
		const log = create_stream_log();
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true, format: 'markdown' }),
			log,
			deps
		);
		assert.strictEqual(outcome, 'done');
		assert.include(steps, 'confirm');
		assert.strictEqual(log.stdout.length, 1);
		assert.ok(log.stdout[0]!.startsWith('# Publishing Result'));
		const errors = log.stderr.join('\n');
		assert.include(errors, 'Publishing Plan');
		assert.include(errors, 'all 2 npm repos are ready to publish');
		assert.include(errors, 'This will publish the packages shown above');
	});

	test('--emit_json --wetrun: stdout carries JSON-lines events alone', async () => {
		const { deps } = create_recording_deps({
			repos: create_gate_repos(),
			fetched_entries: ready_entries()
		});
		const log = create_stream_log();
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true, emit_json: true }),
			log,
			deps
		);
		assert.strictEqual(outcome, 'done');
		assert.ok(log.stdout.length > 1);
		for (const line of log.stdout) {
			assert.notInclude(line, '\n');
			assert.isString(JSON.parse(line).event);
		}
		assert.include(log.stderr.join('\n'), 'Publishing Plan');
	});

	test('the human format logs on stdout', async () => {
		const { deps } = create_recording_deps({ repos: create_gate_repos(), fetched_entries: [] });
		const log = create_stream_log();
		await run_gitops_publish(Args.parse({}), log, deps);
		assert.ok(log.stdout.length > 0);
		assert.ok(log.stdout.every((l) => l.startsWith('[test]')));
	});
});

describe('run_gitops_publish report masking', () => {
	const SECRET_FAILURE = { ok: false as const, message: 'failed: SECRET_NPM_TOKEN=hunter2' };

	const run_failing = async (format: 'json' | 'markdown'): Promise<string> => {
		const { deps } = create_recording_deps({
			repos: create_gate_repos(),
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({ key: 'b' })
			]
		});
		deps.ops = { ...deps.ops, process: { run_interactive: async () => SECRET_FAILURE } };
		const log = create_stream_log();
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true, plan: false, format }),
			log,
			deps
		);
		assert.strictEqual(outcome, 'failed');
		assert.strictEqual(log.stdout.length, 1);
		return log.stdout[0]!;
	};

	test('--format json masks secrets in the events, as the live stream does', async () => {
		const report = JSON.parse(await run_failing('json'));
		const failed = report.events.find((e: { event: string }) => e.event === 'package_failed') as {
			error: string;
		};
		assert.include(failed.error, 'SECRET_NPM_TOKEN=[redacted]');
		assert.notInclude(JSON.stringify(report), 'hunter2');
	});

	test('--format json carries each failure message, masked', async () => {
		const report = JSON.parse(await run_failing('json'));
		assert.isAbove(report.failed.length, 0);
		for (const failure of report.failed as Array<{ name: string; error: unknown }>) {
			assert.isString(failure.name);
			assert.isString(failure.error);
		}
		assert.include(report.failed[0].error, 'SECRET_NPM_TOKEN=[redacted]');
		assert.notInclude(JSON.stringify(report), 'hunter2');
	});

	test('--format json carries a fatal error message, masked', async () => {
		const { deps } = create_recording_deps({
			repos: create_gate_repos(),
			fetched_entries: [
				create_mock_repos_entry({ key: 'a' }),
				create_mock_repos_entry({ key: 'b' })
			]
		});
		deps.ops = {
			...deps.ops,
			preflight: {
				run_preflight_checks: async () => {
					throw new Error('preflight blew up: SECRET_NPM_TOKEN=hunter2');
				}
			}
		};
		const log = create_stream_log();
		const outcome = await run_gitops_publish(
			Args.parse({ wetrun: true, plan: false, format: 'json' }),
			log,
			deps
		);
		assert.strictEqual(outcome, 'failed');
		assert.strictEqual(log.stdout.length, 1);
		const report = JSON.parse(log.stdout[0]!);
		assert.deepEqual(report.failed, [
			{ name: 'FATAL_ERROR', error: 'preflight blew up: SECRET_NPM_TOKEN=[redacted]' }
		]);
		assert.notInclude(log.stdout[0]!, 'hunter2');
	});

	test('--format markdown masks secrets in the failures', async () => {
		const report = await run_failing('markdown');
		assert.include(report, 'SECRET_NPM_TOKEN=[redacted]');
		assert.notInclude(report, 'hunter2');
	});
});

describe('to_child_stdout', () => {
	test('routes the child stdout to stderr when our stdout carries a machine stream', () => {
		const route = (args: Partial<Args>): 'stdout' | 'stderr' => to_child_stdout(Args.parse(args));
		assert.strictEqual(route({}), 'stdout');
		assert.strictEqual(route({ emit_json: true }), 'stderr');
		assert.strictEqual(route({ format: 'json' }), 'stderr');
		assert.strictEqual(route({ format: 'markdown' }), 'stderr');
		// a report sent to a file leaves stdout to the humans
		assert.strictEqual(route({ format: 'json', outfile: 'out.json' }), 'stdout');
		assert.strictEqual(route({ format: 'json', outfile: 'out.json', emit_json: true }), 'stderr');
	});
});

describe('format_failure_markdown', () => {
	test('a one-line message is the bullet alone', () => {
		assert.deepEqual(format_failure_markdown('pkg', 'Failed to read package.json: ENOENT'), [
			'- `pkg`: Failed to read package.json: ENOENT'
		]);
	});

	test('the rest of the message goes in an indented fenced block', () => {
		assert.deepEqual(
			format_failure_markdown(
				'pkg',
				'`gro publish` failed (code 1)\nthe end of its stderr:\nnpm error code E401'
			),
			[
				'- `pkg`: `gro publish` failed (code 1)',
				'',
				'  ```',
				'  the end of its stderr:',
				'  npm error code E401',
				'  ```'
			]
		);
	});

	test('the fence outruns any backtick run in the stderr', () => {
		const lines = format_failure_markdown('pkg', 'failed\n```\nquoted\n```');
		assert.strictEqual(lines[2], '  ````');
		assert.strictEqual(lines.at(-1), '  ````');
	});
});
