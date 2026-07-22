import { assert, test } from 'vitest';
import { assert_rejects } from '@fuzdev/fuz_util/testing.ts';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
	local_repo_load,
	local_repos_load,
	repo_is_npm,
	type LocalRepoPath
} from '$lib/local_repo.ts';
import { create_mock_git_ops, create_mock_npm_ops } from './test_helpers.ts';

const create_local_repo_path = (name: string = 'test-repo'): LocalRepoPath => ({
	type: 'local_repo_path',
	repo_name: name,
	repo_dir: `/test/${name}`,
	repo_url: `https://github.com/test/${name}`,
	repo_git_ssh_url: `git@github.com:test/${name}.git`,
	repo_config: {
		repo_url: `https://github.com/test/${name}`,
		repo_dir: null,
		branch: 'main',
		visibility: 'public',
		ci: true,
		archived: false
	}
});

// -- local_repo_load: operation-level failures --

test('current_commit_hash failure propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_commit_hash: async () => ({ ok: false, message: 'not a git repository' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to get commit hash.*not a git repository/
	);
});

test('current_branch_name failure propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ ok: false, message: 'detached HEAD state' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to get current branch.*detached HEAD/
	);
});

test('has_remote failure propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: false, message: 'git error' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to check for remote.*git error/
	);
});

test('check_clean_workspace failure during branch switch propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ ok: true, value: 'other-branch' }),
					check_clean_workspace: async () => ({ ok: false, message: 'git status failed' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to check workspace.*git status failed/
	);
});

test('check_clean_workspace failure after pull propagates', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					// on correct branch so no branch-switch check — this is the post-pull check
					check_clean_workspace: async () => ({ ok: false, message: 'git status timed out' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to check workspace.*git status timed out/
	);
});

test('post-pull current_commit_hash failure propagates', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					current_commit_hash: async () => {
						call++;
						if (call <= 1) return { ok: true, value: 'aaa' };
						return { ok: false, message: 'corrupt ref' };
					}
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to get commit hash.*corrupt ref/
	);
});

test('has_file_changed failure propagates', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					current_commit_hash: async () => {
						call++;
						return { ok: true, value: call <= 1 ? 'aaa' : 'bbb' };
					},
					has_file_changed: async () => ({ ok: false, message: 'diff failed' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to check if package\.json changed.*diff failed/
	);
});

// -- local_repo_load: pull targets the configured branch --

test('pull is invoked with the configured branch', async () => {
	// Regression: pull was called without a branch, so `git pull origin ''` targeted
	// the remote's default branch (origin/HEAD) and rebased a non-default checkout
	// onto it. The pull must target `repo_config.branch`.
	const local_repo_path = create_local_repo_path();
	local_repo_path.repo_config.branch = 'fuz-app';
	let pulled_branch: string | undefined = 'NOT_CALLED';
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path,
				git_ops: create_mock_git_ops({
					// already on the configured branch, so no checkout — just the pull
					current_branch_name: async () => ({ ok: true, value: 'fuz-app' }),
					has_remote: async () => ({ ok: true, value: true }),
					pull: async (options) => {
						pulled_branch = options?.branch;
						return { ok: true };
					}
				}),
				npm_ops: create_mock_npm_ops()
			}),
		// reaches library-load after a successful pull
		/Failed to load library metadata/
	);
	assert.equal(pulled_branch, 'fuz-app');
});

// -- local_repo_load: behavioral errors --

test('pull failure includes message', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					pull: async () => ({
						ok: false,
						message: 'cannot pull with rebase: You have unstaged changes.'
					})
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to pull in \/test\/test-repo.*unstaged changes/
	);
});

test('checkout failure includes message', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ ok: true, value: 'other-branch' }),
					checkout: async () => ({
						ok: false,
						message: "pathspec 'main' did not match any file(s)"
					})
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to checkout branch "main" in \/test\/test-repo.*pathspec/
	);
});

test('dirty workspace blocks branch switch', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ ok: true, value: 'other-branch' }),
					check_clean_workspace: async () => ({ ok: true, value: false })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/not on branch "main" and the workspace is unclean/
	);
});

test('dirty workspace after pull', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					// on correct branch so no branch-switch check — this is the post-pull check
					check_clean_workspace: async () => ({ ok: true, value: false })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/unclean after pulling branch "main"/
	);
});

test('install failure includes stderr', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					current_commit_hash: async () => {
						call++;
						return { ok: true, value: call <= 1 ? 'aaa' : 'bbb' };
					},
					has_file_changed: async () => ({ ok: true, value: true })
				}),
				npm_ops: create_mock_npm_ops({
					install: async () => ({
						ok: false,
						message: 'Install failed',
						stderr: 'npm ERR! ERESOLVE could not resolve'
					})
				})
			}),
		/Failed to install dependencies[\s\S]*ERESOLVE/
	);
});

// -- local_repo_load: sync: false (read-as-is) --

test('sync: false skips all git operations', async () => {
	// Every git op below would fail if called; reaching library-load proves the
	// whole sync block (branch switch, pull, install, clean checks) was skipped.
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				sync: false,
				git_ops: create_mock_git_ops({
					current_commit_hash: async () => ({ ok: false, message: 'should not be called' }),
					current_branch_name: async () => ({ ok: false, message: 'should not be called' }),
					check_clean_workspace: async () => ({ ok: false, message: 'should not be called' }),
					checkout: async () => ({ ok: false, message: 'should not be called' }),
					pull: async () => ({ ok: false, message: 'should not be called' }),
					has_remote: async () => ({ ok: false, message: 'should not be called' })
				}),
				npm_ops: create_mock_npm_ops({
					install: async () => ({ ok: false, message: 'should not be called' })
				})
			}),
		/Failed to load library metadata/
	);
});

test('sync: false ignores a dirty workspace on the wrong branch', async () => {
	// Dirty + wrong branch would throw when syncing; with sync: false it loads as-is.
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				sync: false,
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ ok: true, value: 'feature-branch' }),
					check_clean_workspace: async () => ({ ok: true, value: false })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		// reaches library-load rather than the dirty/branch guards
		/Failed to load library metadata/
	);
});

// -- local_repo_load: allow_dirty (sync, tolerating uncommitted changes) --

test('allow_dirty lets a dirty branch switch proceed to checkout', async () => {
	// Without allow_dirty this throws "unclean, blocking switch"; with it, the
	// dirty guard is skipped and the (failing) checkout is what surfaces.
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				allow_dirty: true,
				git_ops: create_mock_git_ops({
					current_branch_name: async () => ({ ok: true, value: 'other-branch' }),
					check_clean_workspace: async () => ({ ok: true, value: false }),
					checkout: async () => ({ ok: false, message: 'checkout reached' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to checkout branch "main".*checkout reached/
	);
});

test('allow_dirty tolerates a dirty workspace after pull', async () => {
	// Without allow_dirty this throws "unclean after pulling"; with it, loading continues.
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				allow_dirty: true,
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					check_clean_workspace: async () => ({ ok: true, value: false })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to load library metadata/
	);
});

// -- local_repo_load: skip behaviors --

test('local-only repos skip pull', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: false }),
					// pull would fail if called — reaching library-load error proves it was skipped
					pull: async () => ({ ok: false, message: 'pull should not be called' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to load library metadata/
	);
});

test('skips install when no new commits', async () => {
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true })
					// default mock returns same hash for both calls → no new commits
				}),
				npm_ops: create_mock_npm_ops({
					// install would fail if called — reaching library-load error proves it was skipped
					install: async () => ({ ok: false, message: 'install should not be called' })
				})
			}),
		/Failed to load library metadata/
	);
});

test('skips install when package.json unchanged', async () => {
	let call = 0;
	await assert_rejects(
		() =>
			local_repo_load({
				local_repo_path: create_local_repo_path(),
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					current_commit_hash: async () => {
						call++;
						return { ok: true, value: call <= 1 ? 'aaa' : 'bbb' };
					},
					has_file_changed: async () => ({ ok: true, value: false })
				}),
				npm_ops: create_mock_npm_ops({
					// install would fail if called — reaching library-load error proves it was skipped
					install: async () => ({ ok: false, message: 'install should not be called' })
				})
			}),
		/Failed to load library metadata/
	);
});

// -- local_repos_load --

test('local_repos_load aggregates errors from multiple repos', async () => {
	const err = await assert_rejects(
		() =>
			local_repos_load({
				local_repo_paths: [create_local_repo_path('repo-a'), create_local_repo_path('repo-b')],
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					pull: async () => ({ ok: false, message: 'unstaged changes' })
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to load 2 repos/
	);
	assert.include(err.message, 'repo-a');
	assert.include(err.message, 'repo-b');
});

test('local_repos_load includes per-repo error details', async () => {
	const err = await assert_rejects(
		() =>
			local_repos_load({
				local_repo_paths: [create_local_repo_path('repo-ok'), create_local_repo_path('repo-bad')],
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					pull: async (options) => {
						if (options?.cwd?.includes('repo-bad')) {
							return { ok: false, message: 'unstaged changes' };
						}
						return { ok: true };
					}
				}),
				npm_ops: create_mock_npm_ops()
			}),
		/Failed to load 2 repos/
	);
	// repo-bad fails at pull with specific message
	assert.include(err.message, 'repo-bad');
	assert.include(err.message, 'unstaged changes');
	// repo-ok fails later at library-metadata load
	assert.include(err.message, 'repo-ok');
	assert.include(err.message, 'Failed to load library metadata');
});

test('local_repos_load sequential mode throws on first failure', async () => {
	await assert_rejects(
		() =>
			local_repos_load({
				local_repo_paths: [create_local_repo_path('repo-a'), create_local_repo_path('repo-b')],
				git_ops: create_mock_git_ops({
					has_remote: async () => ({ ok: true, value: true }),
					pull: async () => ({ ok: false, message: 'network error' })
				}),
				npm_ops: create_mock_npm_ops(),
				parallel: false
			}),
		/Failed to pull.*network error/
	);
});

// -- local_repo_load: non-npm (cargo) repos --

/** Builds a `LocalRepoPath` pointing at a real temp dir for the cargo divert tests. */
const cargo_local_repo_path = (repo_dir: string, name = 'rust-repo'): LocalRepoPath => ({
	type: 'local_repo_path',
	repo_name: name,
	repo_dir,
	repo_url: `https://github.com/test/${name}`,
	repo_git_ssh_url: `git@github.com:test/${name}.git`,
	repo_config: {
		repo_url: `https://github.com/test/${name}`,
		repo_dir: null,
		branch: 'main',
		visibility: 'public',
		ci: true,
		archived: false
	}
});

test('loads a workspace Cargo.toml (no package.json) as a cargo repo, falling back to URL', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'gitops-cargo-'));
	try {
		// A workspace root has no `name` and (here) no `repository` — both fall back to the URL.
		writeFileSync(join(dir, 'Cargo.toml'), '[workspace.package]\nversion = "0.4.2"\n');

		const repo = await local_repo_load({
			local_repo_path: cargo_local_repo_path(dir),
			sync: false
		});

		assert.strictEqual(repo.kind, 'cargo');
		assert.strictEqual(repo_is_npm(repo), false);
		assert.strictEqual(repo.library.name, 'rust-repo'); // from URL, not Cargo.toml
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
			local_repo_path: cargo_local_repo_path(dir),
			sync: false
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
			() => local_repo_load({ local_repo_path: cargo_local_repo_path(dir), sync: false }),
			/Failed to load library metadata/
		);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});
