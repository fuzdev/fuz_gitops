import { describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';

import { git_commit } from '$lib/git_operations.ts';

describe('git_commit', () => {
	test('refuses an empty file list before running git', async () => {
		// a cwd that doesn't exist, so a missing guard fails the spawn instead of committing a real index
		await assert_rejects(
			() => git_commit('message', [], { cwd: '/nonexistent/git_commit_test' }),
			/an empty list would commit the whole index/
		);
	});
});
