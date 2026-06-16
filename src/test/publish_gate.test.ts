import {assert, test, describe} from 'vitest';

import {decide_publish_gate, publish_run_failed} from '$lib/publish_gate.ts';

const no_errors = {errors: []};
const with_errors = {errors: ['Production dependency cycle: a → b → a']};

describe('decide_publish_gate', () => {
	test('a dry run always proceeds without prompting (plan/errors ignored)', () => {
		assert.deepEqual(decide_publish_gate({wetrun: false, show_plan: true, plan: no_errors}), {
			action: 'proceed',
		});
		assert.deepEqual(decide_publish_gate({wetrun: false, show_plan: true, plan: with_errors}), {
			action: 'proceed',
		});
		assert.deepEqual(decide_publish_gate({wetrun: false, show_plan: false, plan: no_errors}), {
			action: 'proceed',
		});
	});

	test('a --no-plan real publish proceeds without prompting (executor still gates errors)', () => {
		assert.deepEqual(decide_publish_gate({wetrun: true, show_plan: false, plan: no_errors}), {
			action: 'proceed',
		});
		// even with plan errors: the task does not block here — the executor fail-louds instead
		assert.deepEqual(decide_publish_gate({wetrun: true, show_plan: false, plan: with_errors}), {
			action: 'proceed',
		});
	});

	test('a real publish showing a clean plan requires confirmation', () => {
		assert.deepEqual(decide_publish_gate({wetrun: true, show_plan: true, plan: no_errors}), {
			action: 'confirm',
		});
	});

	test('a real publish showing a plan with errors is blocked before prompting', () => {
		const gate = decide_publish_gate({wetrun: true, show_plan: true, plan: with_errors});
		assert.strictEqual(gate.action, 'blocked');
		assert(gate.action === 'blocked'); // narrow
		assert.match(gate.message, /Cannot proceed/);
	});
});

describe('publish_run_failed', () => {
	test('a successful run with no fatal error does not fail', () => {
		assert.strictEqual(publish_run_failed({ok: true}, null), false);
	});

	test('an unsuccessful result fails', () => {
		assert.strictEqual(publish_run_failed({ok: false}, null), true);
	});

	test('a fatal error fails even when the result is ok', () => {
		assert.strictEqual(publish_run_failed({ok: true}, new Error('circular deps')), true);
	});

	test('both unsuccessful and fatal fails', () => {
		assert.strictEqual(publish_run_failed({ok: false}, new Error('boom')), true);
	});
});
