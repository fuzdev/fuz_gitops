import { assert, describe, test } from 'vitest';

import { Args, run_gitops_analyze, type GitopsAnalyzeDeps } from '$lib/gitops_analyze.task.ts';
import { create_mock_repo, create_stream_log } from './test_helpers.ts';

const create_deps = (): GitopsAnalyzeDeps => ({
	load_repos: async () => ({
		local_repos: [
			create_mock_repo({ name: 'a' }),
			create_mock_repo({ name: 'b', deps: { a: '^1.0.0' } }),
			create_mock_repo({ name: 'c', kind: 'cargo' })
		]
	})
});

describe('run_gitops_analyze machine output', () => {
	test('--format json: stdout carries the analysis document alone', async () => {
		const log = create_stream_log();
		await run_gitops_analyze(Args.parse({ format: 'json' }), log, create_deps());
		assert.strictEqual(log.stdout.length, 1);
		assert.deepEqual(JSON.parse(log.stdout[0]!).publishing_order, ['a', 'b']);
		// the non-npm exclusion note went to stderr
		assert.ok(log.stderr.some((l) => l.includes('excluding 1 non-npm repo(s)')));
	});

	test('--format markdown: stdout carries the markdown alone', async () => {
		const log = create_stream_log();
		await run_gitops_analyze(Args.parse({ format: 'markdown' }), log, create_deps());
		assert.strictEqual(log.stdout.length, 1);
		assert.ok(log.stdout[0]!.startsWith('# Dependency Analysis'));
	});

	test('the human format logs on stdout', async () => {
		const log = create_stream_log();
		await run_gitops_analyze(Args.parse({}), log, create_deps());
		assert.ok(log.stdout.some((l) => l.includes('excluding 1 non-npm repo(s)')));
		assert.ok(log.stdout.some((l) => l.includes('Publishing order:')));
	});
});
