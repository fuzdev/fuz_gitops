import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';

import { gate_publish_readiness } from '$lib/gitops_task_helpers.ts';
import {
	create_gate_repos,
	create_mock_repo,
	create_mock_repos_entry,
	create_mock_repos_ops,
	create_mock_repos_report
} from './test_helpers.ts';

describe('gate_publish_readiness', () => {
	test('fetches the npm repos alone, passing `--registry`', async () => {
		const repos_ops = create_mock_repos_ops(
			create_mock_repos_report(
				[create_mock_repos_entry({ key: 'a' }), create_mock_repos_entry({ key: 'b' })],
				{ fetched: true }
			)
		);
		await gate_publish_readiness({
			local_repos: create_gate_repos(),
			registry: '../r.toml',
			repos_ops
		});
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
				gate_publish_readiness({
					local_repos: create_gate_repos(),
					registry: '../r.toml',
					repos_ops
				}),
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
			() => gate_publish_readiness({ local_repos: create_gate_repos(), repos_ops }),
			/the readiness check failed/
		);
	});
});
