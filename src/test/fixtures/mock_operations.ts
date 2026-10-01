import type { GitopsOperations } from '$lib/operations.ts';
import type { RepoFixtureSet } from './repo_fixture_types.ts';
import { create_fixture_changeset_ops } from './mock_changeset_operations.ts';
import { create_mock_fs_ops, create_mock_gitops_ops } from '../test_helpers.ts';

/**
 * Creates gitops operations for a fixture: its changesets read from the
 * fixture data, an empty fs, and every other operation succeeding
 * (`create_mock_gitops_ops`). The fixture checks plan and dry-run, which read
 * no file; a wetrun reads each published repo's `package.json`, which the
 * empty fs fails as not found rather than reading the default mock's `{}`.
 */
export const create_fixture_gitops_ops = (fixture: RepoFixtureSet): GitopsOperations =>
	create_mock_gitops_ops({
		changeset: create_fixture_changeset_ops(fixture),
		fs: create_mock_fs_ops()
	});
