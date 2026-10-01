import { assert, describe, test } from 'vitest';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { Args, run_gitops_plan, type GitopsPlanDeps } from '$lib/gitops_plan.task.ts';
import type { LocalRepo } from '$lib/local_repo.ts';
import {
	create_mock_gitops_ops,
	create_mock_repo,
	create_mock_repos_entry,
	create_stream_log
} from './test_helpers.ts';

/** Two npm repos, `b` depending on `a` and off its branch, so the readiness block has a line. */
const create_repos = (): Array<LocalRepo> => {
	const a = create_mock_repo({ name: 'a' });
	const b = create_mock_repo({ name: 'b', deps: { a: '^1.0.0' } });
	b.entry = create_mock_repos_entry({
		key: 'b',
		checkouts: [
			{
				...create_mock_repos_entry({ key: 'b' }).checkouts[0]!,
				head: { kind: 'branch', name: 'feature' }
			}
		],
		at_rest: { on_branch: false, clean: true, idle: true, followed: { kind: 'in_sync' } }
	});
	return [a, b];
};

const create_deps = (): GitopsPlanDeps => ({
	load_repos: async () => ({ local_repos: create_repos() }),
	changeset_ops: create_mock_gitops_ops().changeset
});

describe('run_gitops_plan machine output', () => {
	test('--format json: stdout carries the plan document alone', async () => {
		const log = create_stream_log();
		await run_gitops_plan(Args.parse({ format: 'json' }), log, create_deps());
		assert.strictEqual(log.stdout.length, 1);
		const plan = JSON.parse(log.stdout[0]!);
		assert.deepEqual(plan.publishing_order, ['a', 'b']);
		// the progress lines and the readiness block went to stderr
		const errors = log.stderr.join('\n');
		assert.include(errors, 'Generating multi-repo publishing plan');
		assert.include(errors, 'b: on `feature`, not `main`');
	});

	test('--format markdown: stdout carries the markdown alone', async () => {
		const log = create_stream_log();
		await run_gitops_plan(Args.parse({ format: 'markdown' }), log, create_deps());
		assert.strictEqual(log.stdout.length, 1);
		assert.ok(log.stdout[0]!.startsWith('# Publishing Plan'));
	});

	test('--outfile leaves stdout to the log', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'gitops_plan_test_'));
		try {
			const outfile = join(dir, 'plan.json');
			const log = create_stream_log();
			await run_gitops_plan(Args.parse({ format: 'json', outfile }), log, create_deps());
			assert.ok(log.stdout.some((l) => l.includes('Generating multi-repo publishing plan')));
			assert.ok(log.stdout.every((l) => !l.startsWith('{')));
			assert.deepEqual(JSON.parse(await readFile(outfile, 'utf8')).publishing_order, ['a', 'b']);
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});

	test('the human format logs on stdout', async () => {
		const log = create_stream_log();
		await run_gitops_plan(Args.parse({}), log, create_deps());
		assert.ok(log.stdout.some((l) => l.includes('Generating multi-repo publishing plan')));
	});
});
