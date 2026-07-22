import { assert, describe, test } from 'vitest';
import { join } from 'node:path';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';

import { reconcile_ci, repo_has_workflows } from '$lib/ci_reconcile.ts';

describe('reconcile_ci', () => {
	test('flags ci=true with no workflows as missing_ci', () => {
		const drift = reconcile_ci([
			{
				repo_url: 'https://github.com/x/a',
				ci: true,
				has_workflows: false,
				checkable: true,
				archived: false
			}
		]);
		assert.equal(drift.length, 1);
		const [first] = drift;
		assert(first);
		assert.equal(first.kind, 'missing_ci');
	});

	test('flags ci=false with workflows as stray_ci', () => {
		const drift = reconcile_ci([
			{
				repo_url: 'https://github.com/x/b',
				ci: false,
				has_workflows: true,
				checkable: true,
				archived: false
			}
		]);
		assert.equal(drift.length, 1);
		const [first] = drift;
		assert(first);
		assert.equal(first.kind, 'stray_ci');
	});

	test('no drift when declaration matches reality', () => {
		const drift = reconcile_ci([
			{
				repo_url: 'https://github.com/x/c',
				ci: true,
				has_workflows: true,
				checkable: true,
				archived: false
			},
			{
				repo_url: 'https://github.com/x/d',
				ci: false,
				has_workflows: false,
				checkable: true,
				archived: false
			}
		]);
		assert.equal(drift.length, 0);
	});

	test('skips repos that are not checked out locally', () => {
		const drift = reconcile_ci([
			{
				repo_url: 'https://github.com/x/e',
				ci: true,
				has_workflows: false,
				checkable: false,
				archived: false
			}
		]);
		assert.equal(drift.length, 0);
	});

	test('skips archived repos even when they would otherwise drift', () => {
		const drift = reconcile_ci([
			{
				repo_url: 'https://github.com/x/f',
				ci: true,
				has_workflows: false,
				checkable: true,
				archived: true
			},
			{
				repo_url: 'https://github.com/x/g',
				ci: false,
				has_workflows: true,
				checkable: true,
				archived: true
			}
		]);
		assert.equal(drift.length, 0);
	});
});

describe('repo_has_workflows', () => {
	test('returns false when there is no `.github/workflows` directory', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'ci_reconcile_test_'));
		try {
			assert.equal(repo_has_workflows(dir), false);
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});

	test('returns true when a `.yml` workflow is present', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'ci_reconcile_test_'));
		try {
			await mkdir(join(dir, '.github', 'workflows'), { recursive: true });
			await writeFile(join(dir, '.github', 'workflows', 'check.yml'), 'name: check');
			assert.equal(repo_has_workflows(dir), true);
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});

	test('returns true when a `.yaml` workflow is present', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'ci_reconcile_test_'));
		try {
			await mkdir(join(dir, '.github', 'workflows'), { recursive: true });
			await writeFile(join(dir, '.github', 'workflows', 'deploy.yaml'), 'name: deploy');
			assert.equal(repo_has_workflows(dir), true);
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});

	test('returns false when the workflows directory holds no `.yml`/`.yaml` files', async () => {
		const dir = await mkdtemp(join(tmpdir(), 'ci_reconcile_test_'));
		try {
			await mkdir(join(dir, '.github', 'workflows'), { recursive: true });
			await writeFile(join(dir, '.github', 'workflows', 'README.md'), '# workflows');
			assert.equal(repo_has_workflows(dir), false);
		} finally {
			await rm(dir, { recursive: true, force: true });
		}
	});
});
