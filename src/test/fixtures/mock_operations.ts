import type { GitopsOperations } from '$lib/operations.ts';
import type { RepoFixtureSet } from './repo_fixture_types.ts';
import { create_fixture_changeset_ops } from './mock_changeset_operations.ts';
import { create_mock_fs_ops, create_mock_gitops_ops } from '../test_helpers.ts';

/**
 * Creates gitops operations for a fixture: its changesets read from the
 * fixture data, each repo's `package.json` in the in-memory fs, and every other
 * operation succeeding (`create_mock_gitops_ops`). A path the fixture doesn't
 * set reads as not found, so a fixture-setup mistake fails loud.
 */
export const create_fixture_gitops_ops = (fixture: RepoFixtureSet): GitopsOperations => {
	const fs = create_mock_fs_ops();
	for (const repo of fixture.repos) {
		const path = `/fixtures/repos/${fixture.name}/${repo.repo_name}/package.json`;
		fs.set(path, JSON.stringify(repo.package_json, null, '\t'));
	}
	return create_mock_gitops_ops({ changeset: create_fixture_changeset_ops(fixture), fs });
};
