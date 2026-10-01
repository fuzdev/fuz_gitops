import { assert, describe, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
	local_repo_load,
	local_repos_load,
	repo_is_npm,
	type LocalRepoPath
} from '$lib/local_repo.ts';
import { create_mock_repos_entry } from './test_helpers.ts';

/** Builds a `LocalRepoPath` pointing at a real temp dir for the cargo divert tests. */
const cargo_local_repo_path = (repo_dir: string, name = 'rust-repo'): LocalRepoPath => ({
	repo_name: name,
	repo_dir,
	repo_url: `https://github.com/test/${name}`,
	entry: create_mock_repos_entry({ key: name })
});

// -- non-npm (cargo) repos --
describe('local_repo_load', () => {
	test('loads a workspace Cargo.toml (no package.json) as a cargo repo, falling back to the key and URL', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-cargo-'));
		try {
			// A workspace root has no `name` and (here) no `repository` — both fall back to the entry's.
			writeFileSync(join(dir, 'Cargo.toml'), '[workspace.package]\nversion = "0.4.2"\n');

			const repo = await local_repo_load({
				local_repo_path: cargo_local_repo_path(dir)
			});

			assert.strictEqual(repo.kind, 'cargo');
			assert.strictEqual(repo_is_npm(repo), false);
			assert.strictEqual(repo.library.name, 'rust-repo'); // the registry key, not Cargo.toml
			assert.strictEqual(repo.library.repo_url, 'https://github.com/test/rust-repo');
			assert.strictEqual(repo.package_json.version, '0.4.2');
			assert.strictEqual(repo.package_json.private, true);
			// No npm dependency maps on a cargo repo.
			assert.strictEqual(repo.dependencies, undefined);
			assert.strictEqual(repo.dev_dependencies, undefined);
			assert.strictEqual(repo.peer_dependencies, undefined);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});

	test('uses Cargo.toml [package] identity for a single-crate repo', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-cargo-'));
		try {
			writeFileSync(
				join(dir, 'Cargo.toml'),
				'[package]\nname = "my_crate"\nversion = "1.2.3"\ndescription = "does a thing"\nrepository = "https://github.com/owner/my_crate"\n'
			);

			const repo = await local_repo_load({
				local_repo_path: cargo_local_repo_path(dir)
			});

			assert.strictEqual(repo.kind, 'cargo');
			assert.strictEqual(repo.library.name, 'my_crate');
			assert.strictEqual(repo.package_json.version, '1.2.3');
			assert.strictEqual(repo.package_json.description, 'does a thing');
			assert.strictEqual(repo.library.repo_url, 'https://github.com/owner/my_crate');
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});

	test('a repo with neither package.json nor Cargo.toml still fails as a metadata-load error', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'gitops-empty-'));
		try {
			await assert_rejects(
				() => local_repo_load({ local_repo_path: cargo_local_repo_path(dir) }),
				/Failed to load library metadata/
			);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	});
});

describe('local_repos_load', () => {
	test('aggregates the failures, naming each repo', async () => {
		const root = mkdtempSync(join(tmpdir(), 'gitops-load-'));
		try {
			// `ok` loads as a cargo repo; neither `bad-a` nor `bad-b` has a manifest
			for (const name of ['ok', 'bad-a', 'bad-b']) mkdirSync(join(root, name));
			writeFileSync(join(root, 'ok', 'Cargo.toml'), '[package]\nname = "ok"\n');
			const err = await assert_rejects(
				() =>
					local_repos_load({
						local_repo_paths: ['ok', 'bad-a', 'bad-b'].map((name) =>
							cargo_local_repo_path(join(root, name), name)
						)
					}),
				/Failed to load 2 repos/
			);
			assert.include(err.message, 'bad-a: ');
			assert.include(err.message, 'bad-b: ');
			assert.notInclude(err.message, 'ok: ');
			assert.include(err.message, 'Failed to load library metadata');
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	});

	test('sequential mode throws on the first failure', async () => {
		const root = mkdtempSync(join(tmpdir(), 'gitops-load-'));
		try {
			for (const name of ['bad-a', 'bad-b']) mkdirSync(join(root, name));
			const err = await assert_rejects(
				() =>
					local_repos_load({
						local_repo_paths: ['bad-a', 'bad-b'].map((name) =>
							cargo_local_repo_path(join(root, name), name)
						),
						parallel: false
					}),
				/Failed to load library metadata for repo "bad-a"/
			);
			assert.notInclude(err.message, 'bad-b');
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	});
});
