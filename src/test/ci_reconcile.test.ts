import {assert, describe, test} from 'vitest';

import {reconcile_ci} from '$lib/ci_reconcile.js';

describe('reconcile_ci', () => {
	test('flags ci=true with no workflows as missing_ci', () => {
		const drift = reconcile_ci([
			{repo_url: 'https://github.com/x/a', ci: true, has_workflows: false, checkable: true},
		]);
		assert.equal(drift.length, 1);
		const [first] = drift;
		assert(first);
		assert.equal(first.kind, 'missing_ci');
	});

	test('flags ci=false with workflows as stray_ci', () => {
		const drift = reconcile_ci([
			{repo_url: 'https://github.com/x/b', ci: false, has_workflows: true, checkable: true},
		]);
		assert.equal(drift.length, 1);
		const [first] = drift;
		assert(first);
		assert.equal(first.kind, 'stray_ci');
	});

	test('no drift when declaration matches reality', () => {
		const drift = reconcile_ci([
			{repo_url: 'https://github.com/x/c', ci: true, has_workflows: true, checkable: true},
			{repo_url: 'https://github.com/x/d', ci: false, has_workflows: false, checkable: true},
		]);
		assert.equal(drift.length, 0);
	});

	test('skips repos that are not checked out locally', () => {
		const drift = reconcile_ci([
			{repo_url: 'https://github.com/x/e', ci: true, has_workflows: false, checkable: false},
		]);
		assert.equal(drift.length, 0);
	});
});
