import { assert, describe, test } from 'vitest';

import {
	check_publish_readiness,
	format_repo_readiness_problem,
	repo_readiness_at_rest,
	repo_readiness_for_publish
} from '$lib/repo_readiness.ts';
import { ReposSessions, ReposStatusReport, type ReposEntryStatus } from '$lib/repos_status.ts';
import {
	AT_REST,
	create_mock_repos_entry,
	create_mock_repos_report,
	entry_with,
	kinds,
	load_golden
} from './test_helpers.ts';

describe('repo_readiness_at_rest', () => {
	test('an entry at rest has no problems', () => {
		assert.deepEqual(repo_readiness_at_rest(create_mock_repos_entry({ key: 'a' })), []);
	});

	test('off its branch names the head', () => {
		const entry = entry_with('a', {
			checkout: { head: { kind: 'branch', name: 'feature' } },
			at_rest: { ...AT_REST, on_branch: false }
		});
		assert.deepEqual(repo_readiness_at_rest(entry), [
			{ kind: 'off_branch', branch: 'main', head: { kind: 'branch', name: 'feature' } }
		]);
		assert.strictEqual(
			format_repo_readiness_problem('a', repo_readiness_at_rest(entry)[0]!).what,
			'on `feature`, not `main`'
		);
	});

	test('detached names the commit', () => {
		const entry = entry_with('a', {
			checkout: { head: { kind: 'detached', commit: '0123456789abcdef0123' } },
			at_rest: { ...AT_REST, on_branch: false }
		});
		const [problem] = repo_readiness_at_rest(entry);
		assert.strictEqual(
			format_repo_readiness_problem('a', problem!).what,
			'detached at 0123456789ab, not on `main`'
		);
	});

	test('dirty counts each kind, untracked included', () => {
		const uncommitted = { staged: 1, unstaged: 0, untracked: 2, conflicted: 0 };
		const entry = entry_with('a', {
			checkout: { uncommitted },
			at_rest: { ...AT_REST, clean: false }
		});
		const problems = repo_readiness_at_rest(entry);
		assert.deepEqual(problems, [{ kind: 'dirty', uncommitted }]);
		assert.strictEqual(
			format_repo_readiness_problem('a', problems[0]!).what,
			'uncommitted changes (1 staged, 2 untracked)'
		);
		// neutral: after a failed `changeset publish`, committing would drop the changesets
		assert.include(
			format_repo_readiness_problem('a', problems[0]!).fix,
			'commit, stash, or discard them'
		);
	});

	test('an operation in progress names it and how to finish', () => {
		const entry = entry_with('a', {
			checkout: { in_progress: 'rebase' },
			at_rest: { ...AT_REST, idle: false }
		});
		const problems = repo_readiness_at_rest(entry);
		assert.deepEqual(problems, [{ kind: 'in_progress', op: 'rebase' }]);
		const { what, fix } = format_repo_readiness_problem('a', problems[0]!);
		assert.strictEqual(what, 'a rebase is in progress');
		assert.include(fix, 'git rebase --continue');
	});

	test('each followed relation but in_sync is a problem, with its fix', () => {
		const cases: Array<[NonNullable<ReposEntryStatus['at_rest']>['followed'], RegExp, RegExp]> = [
			[{ kind: 'ahead', commits: 2 }, /2 commits ahead of origin/, /`repos sync a` pushes/],
			[{ kind: 'behind', commits: 1 }, /1 commit behind origin/, /`repos sync a` fast-forwards/],
			[{ kind: 'diverged', ahead: 1, behind: 3 }, /diverged .*1 ahead, 3 behind/, /by hand/],
			[{ kind: 'shallow' }, /shallow/, /`repos sync a` moves/],
			[{ kind: 'gone' }, /gone from origin/, /by hand/],
			[{ kind: 'unmapped' }, /refspec/, /refspec/],
			[{ kind: 'untracked' }, /no upstream/, /git branch -u origin\/main main/],
			[null, /isn't compared with origin/, /`repos status a`/]
		];
		for (const [followed, what_re, fix_re] of cases) {
			const problems = repo_readiness_at_rest(
				entry_with('a', { at_rest: { ...AT_REST, followed } })
			);
			assert.deepEqual(problems, [{ kind: 'followed', branch: 'main', relation: followed }]);
			const { what, fix } = format_repo_readiness_problem('a', problems[0]!);
			assert.match(what, what_re);
			assert.match(fix, fix_re);
		}
	});

	test('fixes name `--registry` when the run passed one', () => {
		const problems = repo_readiness_at_rest(
			entry_with('a', { at_rest: { ...AT_REST, followed: { kind: 'behind', commits: 1 } } })
		);
		const { fix } = format_repo_readiness_problem('a', problems[0]!, {
			repos_command: 'repos --registry ../repos.toml'
		});
		assert.include(fix, '`repos --registry ../repos.toml sync a`');
	});

	test('an entry following no branch is a problem', () => {
		const entry = entry_with('a', {
			branch: null,
			at_rest: { on_branch: null, clean: true, idle: true, followed: null }
		});
		assert.deepEqual(kinds(repo_readiness_at_rest(entry)), ['no_branch']);
	});

	test('an unprobed entry is one problem saying why', () => {
		const entry = entry_with('a', {
			at_rest: null,
			probe_error: { kind: 'git_failed', message: 'boom' }
		});
		assert.deepEqual(repo_readiness_at_rest({ ...entry, checkouts: [] }), [
			{ kind: 'unprobed', detail: 'probing failed: boom' }
		]);
	});

	test('several problems come in a fixed order', () => {
		const entry = entry_with('a', {
			checkout: {
				head: { kind: 'branch', name: 'wip' },
				uncommitted: { staged: 0, unstaged: 1, untracked: 0, conflicted: 0 },
				in_progress: 'merge'
			},
			at_rest: {
				on_branch: false,
				clean: false,
				idle: false,
				followed: { kind: 'ahead', commits: 1 }
			}
		});
		assert.deepEqual(kinds(repo_readiness_at_rest(entry)), [
			'off_branch',
			'dirty',
			'in_progress',
			'followed'
		]);
	});
});

describe('repo_readiness_for_publish', () => {
	test('a ready entry has no problems', () => {
		assert.deepEqual(repo_readiness_for_publish(create_mock_repos_entry({ key: 'a' })), []);
	});

	test('ahead of origin is ready, though not at rest', () => {
		const entry = entry_with('a', {
			at_rest: { ...AT_REST, followed: { kind: 'ahead', commits: 3 } }
		});
		assert.deepEqual(kinds(repo_readiness_at_rest(entry)), ['followed']);
		assert.deepEqual(repo_readiness_for_publish(entry), []);
	});

	test('behind, diverged, gone, unmapped, untracked, and uncompared are not ready', () => {
		const relations: Array<NonNullable<ReposEntryStatus['at_rest']>['followed']> = [
			{ kind: 'behind', commits: 1 },
			{ kind: 'diverged', ahead: 1, behind: 1 },
			{ kind: 'gone' },
			{ kind: 'unmapped' },
			{ kind: 'untracked' },
			{ kind: 'shallow' },
			null
		];
		for (const followed of relations) {
			const entry = entry_with('a', { at_rest: { ...AT_REST, followed } });
			assert.deepEqual(kinds(repo_readiness_for_publish(entry)), ['followed']);
		}
	});

	test('a failed fetch is a problem', () => {
		const entry = entry_with('a', {
			fetch_error: { kind: 'timed_out', after_secs: 60 }
		});
		const problems = repo_readiness_for_publish(entry);
		assert.deepEqual(kinds(problems), ['fetch_failed']);
		assert.include(format_repo_readiness_problem('a', problems[0]!).what, 'timed out after 60s');
	});

	test('a live session in the primary checkout is a problem', () => {
		const session = {
			pid: 4242,
			cwd: '/test/a',
			worktree: null,
			process_cwd: null,
			source: 'session_file'
		} as const;
		const problems = repo_readiness_for_publish(entry_with('a', { checkout: { busy: [session] } }));
		assert.deepEqual(problems, [{ kind: 'busy', sessions: [session] }]);
		assert.include(format_repo_readiness_problem('a', problems[0]!).what, 'pid 4242');
	});

	test('a needs_human reason is a problem', () => {
		const reason = {
			kind: 'origin_mismatch',
			origin: { kind: 'no_url' },
			expected: 'git@github.com:test/a.git',
			fix: { kind: 'set_url' }
		} as const;
		const problems = repo_readiness_for_publish(entry_with('a', { needs_human: [reason] }));
		assert.deepEqual(problems, [{ kind: 'needs_human', reason }]);
		assert.include(format_repo_readiness_problem('a', problems[0]!).fix, '`repos status a`');
	});

	test('a reason restating an at-rest problem is left out', () => {
		const entry = entry_with('a', {
			checkout: { in_progress: 'merge', head: { kind: 'detached', commit: 'abc' } },
			at_rest: { ...AT_REST, on_branch: false, idle: false },
			needs_human: [
				{ kind: 'operation_in_progress', checkout: '/test/a', op: 'merge' },
				{ kind: 'unexpected_detached', checkout: '/test/a' },
				// a linked worktree's operation isn't the primary's, so it stays
				{ kind: 'operation_in_progress', checkout: '/test/a-wt', op: 'rebase' }
			]
		});
		const problems = repo_readiness_for_publish(entry);
		assert.deepEqual(kinds(problems), ['off_branch', 'in_progress', 'needs_human']);
		assert.deepEqual(problems[2], {
			kind: 'needs_human',
			reason: { kind: 'operation_in_progress', checkout: '/test/a-wt', op: 'rebase' }
		});
	});

	test('a default_branch reason stands in for the followed problem', () => {
		const entry = entry_with('a', {
			at_rest: { ...AT_REST, followed: null },
			needs_human: [{ kind: 'default_branch_missing', branch: 'main' }]
		});
		assert.deepEqual(repo_readiness_for_publish(entry), [
			{ kind: 'needs_human', reason: { kind: 'default_branch_missing', branch: 'main' } }
		]);
	});

	test('every golden entry reads and formats without throwing', () => {
		const report = ReposStatusReport.parse(load_golden('status_report.json'));
		for (const entry of report.entries) {
			for (const problem of repo_readiness_for_publish(entry)) {
				const { what, fix } = format_repo_readiness_problem(entry.key, problem);
				assert.ok(what.length > 0 && fix.length > 0, `${entry.key}: ${problem.kind}`);
			}
		}
	});
});

describe('check_publish_readiness', () => {
	const fetched = (entries: Array<ReposEntryStatus>) =>
		create_mock_repos_report(entries, { fetched: true });

	test('passes a ready set', () => {
		const report = fetched([
			create_mock_repos_entry({ key: 'a' }),
			create_mock_repos_entry({ key: 'b' })
		]);
		assert.deepEqual(check_publish_readiness({ report, keys: ['a', 'b'] }), {
			ok: true,
			ahead: []
		});
	});

	test('passes a repo ahead of origin, returning it with its count', () => {
		const report = fetched([
			create_mock_repos_entry({ key: 'a' }),
			entry_with('b', { at_rest: { ...AT_REST, followed: { kind: 'ahead', commits: 2 } } })
		]);
		assert.deepEqual(check_publish_readiness({ report, keys: ['a', 'b'] }), {
			ok: true,
			ahead: [{ key: 'b', branch: 'main', commits: 2 }]
		});
	});

	test('a refusal still returns the ready repos ahead', () => {
		const report = fetched([
			entry_with('a', { at_rest: { ...AT_REST, followed: { kind: 'ahead', commits: 1 } } }),
			entry_with('b', { at_rest: { ...AT_REST, followed: { kind: 'behind', commits: 1 } } })
		]);
		const checked = check_publish_readiness({ report, keys: ['a', 'b'] });
		assert.ok(!checked.ok);
		assert.deepEqual(checked.ahead, [{ key: 'a', branch: 'main', commits: 1 }]);
		assert.deepEqual(checked.lines, [
			'b: `main` is 1 commit behind origin — `repos sync b` fast-forwards it'
		]);
	});

	test('refuses naming each repo, what is wrong, and the fix', () => {
		const report = fetched([
			entry_with('a', {
				checkout: { head: { kind: 'branch', name: 'feature' } },
				at_rest: { ...AT_REST, on_branch: false }
			}),
			create_mock_repos_entry({ key: 'b' }),
			entry_with('c', { at_rest: { ...AT_REST, followed: { kind: 'behind', commits: 2 } } })
		]);
		const checked = check_publish_readiness({ report, keys: ['a', 'b', 'c'] });
		assert.ok(!checked.ok);
		assert.deepEqual(
			checked.not_ready.map((r) => r.key),
			['a', 'c']
		);
		assert.include(checked.message, 'nothing was changed');
		assert.include(checked.message, 'a: on `feature`, not `main` — switch to `main`');
		assert.include(
			checked.message,
			'c: `main` is 2 commits behind origin — `repos sync c` fast-forwards it'
		);
		assert.notInclude(checked.message, 'b:');
	});

	test('a dirty repo points at the troubleshooting doc, where a failed publish is covered', () => {
		const report = fetched([
			entry_with('a', {
				checkout: { uncommitted: { staged: 1, unstaged: 0, untracked: 0, conflicted: 0 } },
				at_rest: { ...AT_REST, clean: false }
			})
		]);
		const checked = check_publish_readiness({ report, keys: ['a'] });
		assert.ok(!checked.ok);
		assert.deepEqual(checked.lines, [
			'a: uncommitted changes (1 staged) — commit, stash, or discard them (untracked files count) — after a failed publish, see the troubleshooting doc first'
		]);
	});

	test('refuses a key the report lacks', () => {
		const checked = check_publish_readiness({ report: fetched([]), keys: ['a'] });
		assert.ok(!checked.ok);
		assert.include(checked.message, "a: can't be read: not in the `repos status` report");
	});

	test('refuses a report that was not fetched', () => {
		const report = create_mock_repos_report([create_mock_repos_entry({ key: 'a' })]);
		const checked = check_publish_readiness({ report, keys: ['a'] });
		assert.ok(!checked.ok);
		assert.include(checked.message, "wasn't fetched");
	});

	test('refuses when busy detection is unavailable, for each golden reason', () => {
		const sessions = (load_golden('sessions.json') as Array<unknown>).map((s) =>
			ReposSessions.parse(s)
		);
		const unavailable = sessions.filter((s) => s.kind === 'unavailable');
		assert.ok(unavailable.length > 0);
		for (const s of unavailable) {
			const report = create_mock_repos_report([create_mock_repos_entry({ key: 'a' })], {
				fetched: true,
				sessions: s
			});
			const checked = check_publish_readiness({ report, keys: ['a'] });
			assert.ok(!checked.ok);
			assert.include(checked.message, 'busy detection is unavailable');
		}
	});
});
