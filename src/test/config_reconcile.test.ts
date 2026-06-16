import {assert, describe, test} from 'vitest';
import type {Url} from '@fuzdev/fuz_util/url.ts';

import {normalize_gitops_config, type RawGitopsRepoConfig} from '$lib/gitops_config.ts';
import {reconcile_configs, type NamedRepos} from '$lib/config_reconcile.ts';

const named = (name: string, raw: Array<Url | RawGitopsRepoConfig>): NamedRepos => ({
	name,
	repos: normalize_gitops_config({repos: raw}).repos,
});

describe('reconcile_configs', () => {
	test('no drift when a subset matches the canonical config', () => {
		const canonical = named('canon', [
			'https://github.com/x/pub',
			{repo_url: 'https://github.com/x/priv', visibility: 'private'},
		]);
		const subset = named('sub', ['https://github.com/x/pub']);
		assert.equal(reconcile_configs(canonical, [subset]).length, 0);
	});

	test('repos that live only in the canonical config do not drift', () => {
		const canonical = named('canon', ['https://github.com/x/a', 'https://github.com/x/b']);
		const subset = named('sub', ['https://github.com/x/a']);
		assert.equal(reconcile_configs(canonical, [subset]).length, 0);
	});

	test('flags a repo missing from the canonical config', () => {
		const canonical = named('canon', ['https://github.com/x/a']);
		const subset = named('sub', ['https://github.com/x/b']);
		const drift = reconcile_configs(canonical, [subset]);
		assert.equal(drift.length, 1);
		const [d] = drift;
		assert(d);
		assert.equal(d.kind, 'missing_from_canonical');
		assert.equal(d.config, 'sub');
	});

	test('flags a branch mismatch', () => {
		const canonical = named('canon', ['https://github.com/x/zzz']); // branch defaults to main
		const subset = named('sub', [{repo_url: 'https://github.com/x/zzz', branch: 'fuz-app'}]);
		const drift = reconcile_configs(canonical, [subset]);
		assert.equal(drift.length, 1);
		const [d] = drift;
		assert(d);
		assert.equal(d.kind, 'field_mismatch');
		assert.equal(d.field, 'branch');
		assert.equal(d.canonical_value, 'main');
		assert.equal(d.config_value, 'fuz-app');
	});

	test('flags a visibility mismatch (ci held equal to isolate it)', () => {
		// ci is held equal on both sides; otherwise the visibility difference would
		// also cascade into a derived-ci difference (a second, real drift).
		const canonical = named('canon', [
			{repo_url: 'https://github.com/x/r', visibility: 'private', ci: true},
		]);
		const subset = named('sub', [{repo_url: 'https://github.com/x/r', ci: true}]); // public by default
		const drift = reconcile_configs(canonical, [subset]);
		assert.equal(drift.length, 1);
		const [d] = drift;
		assert(d);
		assert.equal(d.field, 'visibility');
		assert.equal(d.canonical_value, 'private');
		assert.equal(d.config_value, 'public');
	});

	test('flags a ci mismatch', () => {
		const canonical = named('canon', [{repo_url: 'https://github.com/x/r', ci: true}]);
		const subset = named('sub', [{repo_url: 'https://github.com/x/r', ci: false}]); // visibility held equal
		const drift = reconcile_configs(canonical, [subset]);
		assert.equal(drift.length, 1);
		const [d] = drift;
		assert(d);
		assert.equal(d.field, 'ci');
		assert.equal(d.canonical_value, 'true');
		assert.equal(d.config_value, 'false');
	});

	test('flags an archived mismatch', () => {
		const canonical = named('canon', [{repo_url: 'https://github.com/x/r', archived: true}]);
		const subset = named('sub', ['https://github.com/x/r']); // archived defaults to false
		const drift = reconcile_configs(canonical, [subset]);
		assert.equal(drift.length, 1);
		const [d] = drift;
		assert(d);
		assert.equal(d.field, 'archived');
		assert.equal(d.canonical_value, 'true');
		assert.equal(d.config_value, 'false');
	});

	test('attributes drift to each subset independently across multiple subsets', () => {
		const canonical = named('canon', [
			{repo_url: 'https://github.com/x/r', visibility: 'private', ci: true},
		]);
		const ci_subset = named('a', [
			{repo_url: 'https://github.com/x/r', visibility: 'private', ci: false}, // ci drifts
		]);
		const missing_subset = named('b', ['https://github.com/x/absent']); // not in canonical
		const drift = reconcile_configs(canonical, [ci_subset, missing_subset]);
		assert.equal(drift.length, 2);
		const by_config = new Map(drift.map((d) => [d.config, d]));
		assert.equal(by_config.get('a')?.kind, 'field_mismatch');
		assert.equal(by_config.get('a')?.field, 'ci');
		assert.equal(by_config.get('b')?.kind, 'missing_from_canonical');
	});

	test('a visibility difference cascades into a derived-ci drift', () => {
		const canonical = named('canon', [{repo_url: 'https://github.com/x/r', visibility: 'private'}]);
		const subset = named('sub', ['https://github.com/x/r']); // public + ci:true by default
		const drift = reconcile_configs(canonical, [subset]);
		assert.equal(drift.length, 2);
		assert.deepEqual(
			drift.map((d) => d.field).sort((a, b) => (a ?? '').localeCompare(b ?? '')),
			['ci', 'visibility'],
		);
	});
});
