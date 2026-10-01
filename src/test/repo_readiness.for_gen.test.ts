import { assert, describe, test } from 'vitest';

import { check_gen_readiness, repo_readiness_for_gen } from '$lib/repo_readiness.ts';
import { ReposStatusReport } from '$lib/repos_status.ts';
import {
	AT_REST,
	create_mock_repos_entry,
	create_mock_repos_report,
	entry_with,
	kinds,
	load_golden
} from './test_helpers.ts';

describe('repo_readiness_for_gen', () => {
	const OFF_BRANCH_DIRTY_REBASING = entry_with('a', {
		checkout: {
			head: { kind: 'branch', name: 'feature' },
			uncommitted: { staged: 0, unstaged: 1, untracked: 0, conflicted: 0 },
			in_progress: 'rebase'
		},
		at_rest: { on_branch: false, clean: false, idle: false, followed: { kind: 'in_sync' } }
	});

	test('a ready entry has no problems', () => {
		assert.deepEqual(repo_readiness_for_gen(create_mock_repos_entry({ key: 'a' })), {
			refused: [],
			warned: []
		});
	});

	test('off its branch, dirty, or mid-operation refuses', () => {
		const { refused, warned } = repo_readiness_for_gen(OFF_BRANCH_DIRTY_REBASING);
		assert.deepEqual(kinds(refused), ['off_branch', 'dirty', 'in_progress']);
		assert.deepEqual(warned, []);
	});

	test('allow_dirty warns instead', () => {
		const { refused, warned } = repo_readiness_for_gen(OFF_BRANCH_DIRTY_REBASING, {
			allow_dirty: true
		});
		assert.deepEqual(refused, []);
		assert.deepEqual(kinds(warned), ['off_branch', 'dirty', 'in_progress']);
	});

	test('each followed relation but in_sync warns, as does a failed fetch', () => {
		for (const followed of [
			{ kind: 'behind', commits: 1 },
			{ kind: 'ahead', commits: 1 },
			{ kind: 'diverged', ahead: 1, behind: 1 },
			{ kind: 'gone' },
			null
		] as const) {
			const { refused, warned } = repo_readiness_for_gen(
				entry_with('a', { at_rest: { ...AT_REST, followed } })
			);
			assert.deepEqual(refused, []);
			assert.deepEqual(kinds(warned), ['followed']);
		}
		const { refused, warned } = repo_readiness_for_gen(
			entry_with('a', { fetch_error: { kind: 'timed_out', after_secs: 60 } })
		);
		assert.deepEqual(refused, []);
		assert.deepEqual(kinds(warned), ['fetch_failed']);
	});

	test('busy sessions and needs_human reasons are left out', () => {
		const entry = entry_with('a', {
			checkout: {
				busy: [
					{
						pid: 4242,
						cwd: '/test/a',
						worktree: null,
						process_cwd: null,
						source: 'session_file'
					}
				]
			},
			needs_human: [{ kind: 'unexpected_detached', checkout: '/test/a/wt' }]
		});
		assert.deepEqual(repo_readiness_for_gen(entry), { refused: [], warned: [] });
	});

	test('an unprobed entry refuses, even with allow_dirty', () => {
		const entry = { ...create_mock_repos_entry({ key: 'a' }), at_rest: null, checkouts: [] };
		assert.deepEqual(kinds(repo_readiness_for_gen(entry, { allow_dirty: true }).refused), [
			'unprobed'
		]);
	});

	test('an entry following no branch refuses, even with allow_dirty', () => {
		const entry = entry_with('a', {
			branch: null,
			at_rest: { on_branch: null, clean: true, idle: true, followed: null }
		});
		assert.deepEqual(kinds(repo_readiness_for_gen(entry, { allow_dirty: true }).refused), [
			'no_branch'
		]);
	});
});

describe('check_gen_readiness', () => {
	test('refuses naming each repo and the fix, and still returns the warnings', () => {
		const now = 1_000_000;
		const report = create_mock_repos_report([
			entry_with('a', {
				checkout: { head: { kind: 'branch', name: 'feature' } },
				at_rest: { ...AT_REST, on_branch: false }
			}),
			entry_with('b', {
				at_rest: { ...AT_REST, followed: { kind: 'behind', commits: 2 } },
				fetched_at: now - 120
			}),
			create_mock_repos_entry({ key: 'c' })
		]);
		const checked = check_gen_readiness({ report, keys: ['a', 'b', 'c'], now });
		assert.ok(!checked.ok);
		assert.deepEqual(checked.lines, [
			'a: on `feature`, not `main` — switch to `main` once the work there is committed or stashed'
		]);
		assert.include(checked.message, '`--allow_dirty`');
		assert.deepEqual(checked.warnings, [
			'b: `main` is 2 commits behind origin (fetched 2m ago) — `repos sync b` fast-forwards it'
		]);
	});

	test('allow_dirty passes, warning on each problem', () => {
		const report = create_mock_repos_report([
			entry_with('a', {
				checkout: { uncommitted: { staged: 0, unstaged: 0, untracked: 3, conflicted: 0 } },
				at_rest: { ...AT_REST, clean: false }
			})
		]);
		const checked = check_gen_readiness({ report, keys: ['a'], allow_dirty: true });
		assert.ok(checked.ok);
		assert.strictEqual(checked.warnings.length, 1);
		assert.include(checked.warnings[0], 'a: uncommitted changes (3 untracked)');
	});

	test('refuses a key the report lacks', () => {
		const checked = check_gen_readiness({
			report: create_mock_repos_report([]),
			keys: ['a'],
			allow_dirty: true
		});
		assert.ok(!checked.ok);
		assert.include(checked.message, "a: can't be read: not in the `repos status` report");
	});

	test('every golden entry reads and formats without throwing', () => {
		const report = ReposStatusReport.parse(load_golden('status_report.json'));
		const keys = report.entries.map((e) => e.key);
		for (const allow_dirty of [false, true]) {
			const checked = check_gen_readiness({ report, keys, allow_dirty });
			const lines = [...checked.warnings, ...(checked.ok ? [] : checked.lines)];
			for (const line of lines) assert.notInclude(line, 'undefined');
		}
	});
});
