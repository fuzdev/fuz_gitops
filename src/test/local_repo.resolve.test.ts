import { assert, describe, test } from 'vitest';

import { local_repos_resolve } from '$lib/local_repo.ts';
import { ReposStatusReport } from '$lib/repos_status.ts';
import { create_mock_repos_entry, create_mock_repos_report, load_golden } from './test_helpers.ts';

const load_report = (name: string): ReposStatusReport => ReposStatusReport.parse(load_golden(name));

// the full report: present repos, a reference, a missing entry, one not a repo, a failed probe
const report = load_report('status_report.json');

/** Resolves `keys`, returning the problems it reports. */
const problems_of = (...args: Parameters<typeof local_repos_resolve>): Array<string> => {
	const result = local_repos_resolve(...args);
	assert.ok(!result.ok, 'expected the resolve to fail');
	return result.problems;
};

describe('local_repos_resolve', () => {
	test('resolves keys in config order, not the report order', () => {
		const targeted = load_report('status_report_targeted.json');
		assert.deepEqual(
			targeted.entries.filter((e) => e.kind === 'repo').map((e) => e.key),
			['gro', 'fuz_util']
		);
		const result = local_repos_resolve({ keys: ['fuz_util', 'gro'], report: targeted });
		assert.ok(result.ok);
		assert.deepEqual(
			result.value.map((r) => r.repo_name),
			['fuz_util', 'gro']
		);
	});

	test('each repo holds its entry whole, with its dir under the workspace', () => {
		const result = local_repos_resolve({ keys: ['zzz', 'app'], report });
		assert.ok(result.ok);
		const [zzz, app] = result.value;
		assert.ok(zzz && app);
		assert.strictEqual(
			zzz.entry,
			report.entries.find((e) => e.key === 'zzz')
		);
		assert.strictEqual(zzz.repo_dir, '/home/me/dev/zzz');
		assert.strictEqual(zzz.repo_url, 'https://github.com/me/zzz');
		assert.strictEqual(zzz.entry.branch, 'dev');
		assert.strictEqual(app.repo_dir, '/home/me/dev/app');
	});

	test('the dir comes from the workspace and the entry, whatever the key', () => {
		const entry = create_mock_repos_entry({ key: 'fuz_repos', dir: 'gitops_checkout' });
		const result = local_repos_resolve({
			keys: ['fuz_repos'],
			report: create_mock_repos_report([entry], { workspace: '/w' })
		});
		assert.ok(result.ok);
		assert.strictEqual(result.value[0]?.repo_dir, '/w/gitops_checkout');
	});

	test('a reference is refused', () => {
		assert.deepEqual(problems_of({ keys: ['test262'], report }), [
			'`test262` is a third-party reference, not an owned repo'
		]);
	});

	test('a missing repo points at `repos sync`', () => {
		assert.deepEqual(problems_of({ keys: ['blake3'], report }), [
			'`blake3` is missing at /home/me/dev/blake3 — `repos sync blake3` clones it'
		]);
	});

	test('a dir that is not a repo is refused', () => {
		assert.deepEqual(problems_of({ keys: ['goblins'], report }), [
			"`goblins`: /home/me/dev/goblins isn't a git repo"
		]);
	});

	test('a failed probe is refused with its error', () => {
		const [problem] = problems_of({ keys: ['wpt'], report });
		assert.include(problem, '`wpt`: probing /home/me/dev/wpt failed:');
		assert.include(problem, 'bad tree object HEAD');
	});

	test('a dir name listed in place of its key', () => {
		const entry = create_mock_repos_entry({ key: 'fuz_repos', dir: 'gitops_checkout' });
		assert.deepEqual(
			problems_of({ keys: ['gitops_checkout'], report: create_mock_repos_report([entry]) }),
			['`gitops_checkout` is the dir of `fuz_repos`, not a registry key — list `fuz_repos`']
		);
	});

	test('a key the report lacks', () => {
		const [problem] = problems_of({ keys: ['nope'], report });
		assert.include(problem, '`nope`');
	});

	test('every bad entry is named at once, and the good ones pass', () => {
		const result = local_repos_resolve({
			keys: ['app', 'blake3', 'goblins', 'test262', 'wpt', 'fuz_app'],
			report
		});
		assert.ok(!result.ok);
		assert.strictEqual(result.problems.length, 4);
		for (const key of ['blake3', 'goblins', 'test262', 'wpt']) {
			assert.include(result.message, `\`${key}\``);
		}
		assert.notInclude(result.message, '`app`');
		assert.notInclude(result.message, '`fuz_app`');
	});

	describe('a public host refuses private repos', () => {
		const keys = ['app', 'old', 'fuz_forge'];

		test('a public host names each private repo', () => {
			assert.deepEqual(problems_of({ keys, report, host: { name: 'site', private: false } }), [
				'`old` is private, and site is a public package whose generated repos.json would publish its metadata',
				'`fuz_forge` is private, and site is a public package whose generated repos.json would publish its metadata'
			]);
		});

		test('a private host takes them', () => {
			const result = local_repos_resolve({ keys, report, host: { name: 'site', private: true } });
			assert.ok(result.ok);
			assert.strictEqual(result.value.length, 3);
		});

		test('no host checks nothing', () => {
			assert.ok(local_repos_resolve({ keys, report }).ok);
		});
	});
});
