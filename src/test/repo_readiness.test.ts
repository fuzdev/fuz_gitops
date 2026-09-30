import { assert, describe, test } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
	check_gen_readiness,
	check_publish_readiness,
	format_readiness_ahead,
	format_readiness_block,
	format_repo_readiness_problem,
	repo_readiness_at_rest,
	repo_readiness_for_gen,
	repo_readiness_for_publish,
	repos_not_at_rest,
	type RepoReadinessProblem
} from '$lib/repo_readiness.ts';
import {
	ReposSessions,
	ReposStatusReport,
	type ReposCheckout,
	type ReposEntryStatus
} from '$lib/repos_status.ts';
import { create_mock_repos_entry, create_mock_repos_report } from './test_helpers.ts';

// written by the Rust side (`crates/fuz_repos/tests/golden.rs`), never by hand
const GOLDEN_DIR = join(dirname(fileURLToPath(import.meta.url)), 'fixtures/repos_status');
const load_golden = (name: string): unknown =>
	JSON.parse(readFileSync(join(GOLDEN_DIR, name), 'utf8'));

/** An entry `key` whose primary checkout overrides `checkout`, with `at_rest` set whole. */
const entry_with = (
	key: string,
	options: {
		checkout?: Partial<ReposCheckout>;
		at_rest?: ReposEntryStatus['at_rest'];
	} & Partial<Omit<ReposEntryStatus, 'at_rest' | 'checkouts'>>
): ReposEntryStatus => {
	const { checkout, at_rest, ...rest } = options;
	const base = create_mock_repos_entry({ key });
	const primary = { ...base.checkouts[0]!, path: `/test/${key}`, ...checkout };
	return {
		...base,
		...rest,
		checkouts: [primary],
		at_rest: at_rest === undefined ? base.at_rest : at_rest
	};
};

const AT_REST = {
	on_branch: true,
	clean: true,
	idle: true,
	followed: { kind: 'in_sync' }
} as const;

const kinds = (problems: Array<RepoReadinessProblem>): Array<string> => problems.map((p) => p.kind);

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
		const entry = entry_with('a', { at_rest: null, probe_error: 'boom' });
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
		const { refused, warned } = repo_readiness_for_gen(OFF_BRANCH_DIRTY_REBASING, true);
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
		assert.deepEqual(kinds(repo_readiness_for_gen(entry, true).refused), ['unprobed']);
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

describe('format_readiness_ahead', () => {
	test('says whether the release push carries the commits', () => {
		const ahead = { key: 'fuz_ui', branch: 'main', commits: 2 };
		assert.strictEqual(
			format_readiness_ahead(ahead, true),
			'fuz_ui: `main` is 2 commits ahead of origin — publishing pushes them with the release'
		);
		assert.strictEqual(
			format_readiness_ahead(ahead, false),
			"fuz_ui: `main` is 2 commits ahead of origin — it doesn't publish, so they stay unpushed until `repos sync` or `repos push`"
		);
	});
});

describe('format_readiness_block', () => {
	test('no lines when every repo is at rest', () => {
		const not_ready = repos_not_at_rest([create_mock_repos_entry({ key: 'a' })]);
		assert.deepEqual(format_readiness_block(not_ready, 0), []);
	});

	test('a line per problem, with how long ago a relation was fetched', () => {
		const now = 1_000_000;
		const not_ready = repos_not_at_rest([
			create_mock_repos_entry({ key: 'ok' }),
			entry_with('feature', {
				checkout: { head: { kind: 'branch', name: 'repos-tool' } },
				at_rest: { ...AT_REST, on_branch: false }
			}),
			entry_with('stale', {
				at_rest: { ...AT_REST, followed: { kind: 'behind', commits: 3 } },
				fetched_at: now - 7200
			}),
			entry_with('dirty', {
				checkout: { uncommitted: { staged: 0, unstaged: 0, untracked: 1, conflicted: 0 } },
				at_rest: { ...AT_REST, clean: false, followed: { kind: 'ahead', commits: 1 } }
			})
		]);
		assert.deepEqual(format_readiness_block(not_ready, now), [
			'not at rest, so read as they sit (a real publish refuses all but a branch ahead of origin):',
			'  feature: on `repos-tool`, not `main`',
			'  stale: `main` is 3 commits behind origin (fetched 2h ago)',
			'  dirty: uncommitted changes (1 untracked)',
			'  dirty: `main` is 1 commit ahead of origin (unpushed) (never fetched)'
		]);
	});
});
