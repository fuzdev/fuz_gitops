import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { TaskError } from '@fuzdev/gro';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
	Args,
	run_gitops_run,
	type GitopsRunCommandOutput,
	type GitopsRunDeps,
	type GitopsRunResult
} from '$lib/gitops_run.task.ts';
import {
	create_mock_repos_entry,
	create_ready_repos_ops,
	create_stream_log
} from './test_helpers.ts';

// lists the `basic_publishing` fixture's repos by key: repo_a … repo_e
const CONFIG = join(
	dirname(fileURLToPath(import.meta.url)),
	'fixtures/configs/basic_publishing.config.ts'
);
const KEYS = ['repo_a', 'repo_b', 'repo_c', 'repo_d', 'repo_e'];

const OK: GitopsRunCommandOutput = {
	outcome: { kind: 'exited', code: 0 },
	stdout: 'ok\n',
	stderr: ''
};

/**
 * Deps whose `run_command` answers by the repo dir's last segment (the key),
 * `OK` for any key not in `outputs`, and records each call.
 */
const create_deps = (
	outputs: Record<string, GitopsRunCommandOutput | Error> = {},
	entries: Parameters<typeof create_ready_repos_ops>[0] = {}
): GitopsRunDeps & { calls: Array<{ command: string; cwd: string }> } => {
	const calls: Array<{ command: string; cwd: string }> = [];
	return {
		calls,
		repos_ops: create_ready_repos_ops(entries),
		run_command: async (options) => {
			calls.push(options);
			const output = outputs[options.cwd.split('/').pop()!] ?? OK;
			if (output instanceof Error) throw output;
			return output;
		}
	};
};

const parse_json_output = (
	stdout: Array<string>
): {
	command: string;
	concurrency: number;
	repos: Array<GitopsRunResult>;
	summary: { total: number; success: number; failure: number; duration_ms: number };
} => {
	assert.strictEqual(stdout.length, 1, 'stdout carries the JSON document alone');
	return JSON.parse(stdout[0]!);
};

describe('run_gitops_run', () => {
	test('runs the command in every repo, in its dir', async () => {
		const deps = create_deps();
		const log = create_stream_log();
		await run_gitops_run(Args.parse({ _: ['git', 'status'], config: CONFIG }), log, deps);
		assert.sameMembers(
			deps.calls.map((c) => c.cwd),
			KEYS.map((k) => `/test/${k}`)
		);
		assert.ok(deps.calls.every((c) => c.command === 'git status'));
		assert.ok(log.stdout.some((l) => l.includes('All 5 repos succeeded')));
	});

	test('refuses an empty command before resolving the repos', async () => {
		const deps = create_deps();
		await assert_rejects(
			() => run_gitops_run(Args.parse({ config: CONFIG }), create_stream_log(), deps),
			/No command provided/
		);
		assert.deepEqual(deps.calls, []);
	});

	test('refuses a missing repo, naming it, and runs nothing', async () => {
		const deps = create_deps(
			{},
			{ repo_c: create_mock_repos_entry({ key: 'repo_c', presence: { kind: 'missing' } }) }
		);
		const err = await assert_rejects(() =>
			run_gitops_run(Args.parse({ _: ['true'], config: CONFIG }), create_stream_log(), deps)
		);
		assert.ok(err instanceof TaskError);
		assert.include(err.message, '`repo_c` is missing at /test/repo_c');
		assert.deepEqual(deps.calls, []);
	});

	test('a non-zero exit fails the run, reporting its code, after running the rest', async () => {
		const deps = create_deps({
			repo_b: { outcome: { kind: 'exited', code: 3 }, stdout: '', stderr: 'bad\n' }
		});
		const log = create_stream_log();
		const err = await assert_rejects(() =>
			run_gitops_run(Args.parse({ _: ['npm', 'test'], config: CONFIG, format: 'json' }), log, deps)
		);
		assert.ok(err instanceof TaskError);
		assert.strictEqual(err.message, '1 repos failed');
		assert.strictEqual(deps.calls.length, 5);
		const output = parse_json_output(log.stdout);
		const b = output.repos.find((r) => r.repo_name === 'repo_b')!;
		assert.strictEqual(b.status, 'failure');
		assert.strictEqual(b.exit_code, 3);
		assert.strictEqual(b.signal, null);
		assert.strictEqual(b.stderr, 'bad\n');
		assert.deepEqual(
			{ ...output.summary, duration_ms: 0 },
			{ total: 5, success: 4, failure: 1, duration_ms: 0 }
		);
	});

	test('a signal or a spawn error fails with no exit code, never 0', async () => {
		const deps = create_deps({
			repo_a: { outcome: { kind: 'signaled', signal: 'SIGTERM' }, stdout: '', stderr: '' },
			repo_b: { outcome: { kind: 'error', message: 'spawn sh ENOENT' }, stdout: '', stderr: '' },
			repo_c: new Error('runner threw')
		});
		const log = create_stream_log();
		await assert_rejects(
			() => run_gitops_run(Args.parse({ _: ['true'], config: CONFIG, format: 'json' }), log, deps),
			/3 repos failed/
		);
		const by_name = new Map(parse_json_output(log.stdout).repos.map((r) => [r.repo_name, r]));
		const a = by_name.get('repo_a')!;
		assert.strictEqual(a.status, 'failure');
		assert.strictEqual(a.exit_code, null);
		assert.strictEqual(a.signal, 'SIGTERM');
		assert.strictEqual(a.error, undefined);
		const b = by_name.get('repo_b')!;
		assert.strictEqual(b.status, 'failure');
		assert.strictEqual(b.exit_code, null);
		assert.strictEqual(b.error, 'spawn sh ENOENT');
		// a runner that throws is reported under its own repo
		const c = by_name.get('repo_c')!;
		assert.strictEqual(c.status, 'failure');
		assert.strictEqual(c.repo_dir, '/test/repo_c');
		assert.strictEqual(c.exit_code, null);
		assert.strictEqual(c.error, 'runner threw');
	});

	test('the text format reports a signal and an exit code on stderr', async () => {
		const deps = create_deps({
			repo_a: { outcome: { kind: 'signaled', signal: 'SIGKILL' }, stdout: '', stderr: '' },
			repo_b: { outcome: { kind: 'exited', code: 2 }, stdout: '', stderr: '' }
		});
		const log = create_stream_log();
		await assert_rejects(() =>
			run_gitops_run(Args.parse({ _: ['true'], config: CONFIG }), log, deps)
		);
		const errors = log.stderr.join('\n');
		assert.include(errors, 'Killed by SIGKILL');
		assert.include(errors, 'Exit code: 2');
	});

	test('--format json writes one document with every repo to stdout, the log to stderr', async () => {
		const deps = create_deps();
		const log = create_stream_log();
		await run_gitops_run(
			Args.parse({ _: ['echo', 'hi'], config: CONFIG, format: 'json', concurrency: 2 }),
			log,
			deps
		);
		const output = parse_json_output(log.stdout);
		assert.strictEqual(output.command, 'echo hi');
		assert.strictEqual(output.concurrency, 2);
		assert.sameMembers(
			output.repos.map((r) => r.repo_name),
			KEYS
		);
		for (const repo of output.repos) {
			assert.deepEqual(Object.keys(repo).sort(), [
				'duration_ms',
				'exit_code',
				'repo_dir',
				'repo_name',
				'signal',
				'status',
				'stderr',
				'stdout'
			]);
			assert.strictEqual(repo.status, 'success');
			assert.strictEqual(repo.exit_code, 0);
			assert.strictEqual(repo.stdout, 'ok\n');
		}
		assert.strictEqual(output.summary.success, 5);
		// the human lines went to stderr
		assert.ok(log.stderr.some((l) => l.includes('Running echo hi across 5 repos')));
	});
});
