import { assert, describe, test } from 'vitest';

import { fetch_repo_data } from '$lib/fetch_repo_data.ts';
import type { LocalRepo } from '$lib/local_repo.ts';
import { create_mock_repo, create_mock_repos_entry, create_stream_log } from './test_helpers.ts';

const json_response = (body: unknown, status = 200): Response =>
	new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

/**
 * A `fetch` answering each repo's pulls with none and its check runs from
 * `check_runs` by repo name (a status number fails the request), recording
 * each URL.
 */
const create_fetch = (
	check_runs: Record<string, Array<{ status: string; conclusion: string | null }> | number>
): typeof globalThis.fetch & { urls: Array<string> } => {
	const urls: Array<string> = [];
	const fetch = async (input: string | URL | Request): Promise<Response> => {
		const url = String(input instanceof Request ? input.url : input);
		urls.push(url);
		if (url.endsWith('/pulls')) return json_response([]);
		const name = /\/repos\/test\/([^/]+)\//.exec(url)![1]!;
		const runs = check_runs[name] ?? [];
		if (typeof runs === 'number') return json_response({ message: 'nope' }, runs);
		return json_response({ total_count: runs.length, check_runs: runs });
	};
	return Object.assign(fetch, { urls }) as typeof globalThis.fetch & { urls: Array<string> };
};

const create_repo = (name: string, ci = true): LocalRepo => {
	const repo = create_mock_repo({ name });
	repo.entry = create_mock_repos_entry({ key: name, ci });
	return repo;
};

describe('fetch_repo_data CI status', () => {
	test('a repo without CI is not asked for check runs, and nothing is logged', async () => {
		const fetch = create_fetch({});
		const log = create_stream_log();
		const [repo] = await fetch_repo_data({
			local_repos: [create_repo('a', false)],
			log,
			delay: 0,
			fetch
		});
		assert.strictEqual(repo!.check_runs, null);
		assert.ok(fetch.urls.every((u) => !u.includes('check-runs')));
		assert.deepEqual(log.stderr, []);
	});

	test('a branch with no check runs is null, not a failure', async () => {
		const log = create_stream_log();
		const [repo] = await fetch_repo_data({
			local_repos: [create_repo('a')],
			log,
			delay: 0,
			fetch: create_fetch({ a: [] })
		});
		assert.strictEqual(repo!.check_runs, null);
		assert.deepEqual(log.stderr, []);
	});

	test('check runs reduce to one status, read for the entry branch', async () => {
		const fetch = create_fetch({ a: [{ status: 'completed', conclusion: 'success' }] });
		const [repo] = await fetch_repo_data({
			local_repos: [create_repo('a')],
			delay: 0,
			fetch
		});
		assert.deepEqual(repo!.check_runs, { status: 'completed', conclusion: 'success' });
		assert.ok(fetch.urls.includes('https://api.github.com/repos/test/a/commits/main/check-runs'));
	});

	test('a failed fetch is logged as an error and the rest still fetch', async () => {
		const log = create_stream_log();
		const repos = await fetch_repo_data({
			local_repos: [create_repo('a'), create_repo('b')],
			log,
			delay: 0,
			fetch: create_fetch({ a: 500, b: [{ status: 'completed', conclusion: 'success' }] })
		});
		assert.strictEqual(repos[0]!.check_runs, null);
		assert.isNotNull(repos[1]!.check_runs);
		assert.ok(log.stderr.some((l) => l.includes('failed to fetch CI status (500)')));
	});
});
