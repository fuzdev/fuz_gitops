import { assert, test, describe } from 'vitest';

import { summarize_events, type PublishingEvent } from '$lib/publishing_event.ts';
import {
	null_handler,
	capture_handler,
	multi_handler,
	masking_handler,
	stdout_handler,
	redact_secrets,
	mask_secrets
} from '$lib/publishing_event_handler.ts';

const run_started: PublishingEvent = { event: 'run_started', wetrun: false, total: 3 };
const completed: PublishingEvent = {
	event: 'package_completed',
	name: 'a',
	old_version: '1.0.0',
	new_version: '1.0.1',
	bump_type: 'patch',
	breaking: false,
	commit: 'simulated',
	tag: 'v1.0.1'
};

describe('event handlers', () => {
	test('capture_handler collects events in order', () => {
		const capture = capture_handler();
		capture.emit(run_started);
		capture.emit(completed);
		assert.strictEqual(capture.events.length, 2);
		assert.strictEqual(capture.events[0]!.event, 'run_started');
		assert.strictEqual(capture.events[1]!.event, 'package_completed');
	});

	test('null_handler drops events without throwing', () => {
		const handler = null_handler();
		handler.emit(run_started); // should be a no-op
		assert.ok(handler);
	});

	test('multi_handler fans out to every handler', () => {
		const a = capture_handler();
		const b = capture_handler();
		const handler = multi_handler([a, b]);
		handler.emit(run_started);
		assert.strictEqual(a.events.length, 1);
		assert.strictEqual(b.events.length, 1);
	});

	test('stdout_handler writes one tagged JSON object per line', () => {
		const written: Array<string> = [];
		const original = process.stdout.write.bind(process.stdout);
		process.stdout.write = (chunk: string | Uint8Array): boolean => {
			written.push(String(chunk));
			return true;
		};
		try {
			const handler = stdout_handler();
			handler.emit(run_started);
			handler.emit(completed);
		} finally {
			process.stdout.write = original;
		}
		assert.strictEqual(written.length, 2);
		assert.ok(written[0]!.endsWith('\n')); // JSON-lines: one object per line
		const parsed = JSON.parse(written[0]!);
		assert.strictEqual(parsed.event, 'run_started');
		assert.strictEqual(parsed.wetrun, false);
	});

	test('masking_handler redacts secrets before forwarding', () => {
		const inner = capture_handler();
		const handler = masking_handler(inner);
		handler.emit({
			event: 'package_failed',
			name: 'pkg-a',
			error: 'publish failed: SECRET_NPM_TOKEN=hunter2',
			code: 'auth'
		});
		const event = inner.events[0]!;
		assert.strictEqual(event.event, 'package_failed');
		assert(event.event === 'package_failed'); // narrow
		assert.strictEqual(event.error, 'publish failed: SECRET_NPM_TOKEN=[redacted]');
		assert.strictEqual(event.name, 'pkg-a'); // non-secret field untouched
	});
});

describe('redact_secrets', () => {
	test('redacts npm auth tokens (registry-scoped and bare)', () => {
		assert.strictEqual(
			redact_secrets('//registry.npmjs.org/:_authToken=abcd1234secretvalue'),
			'//registry.npmjs.org/:_authToken=[redacted]'
		);
		assert.strictEqual(redact_secrets('_authToken=plainsecret'), '_authToken=[redacted]');
	});

	test('redacts SECRET_* env assignments', () => {
		assert.strictEqual(
			redact_secrets('export SECRET_GITHUB_API_TOKEN=ghp_example'),
			'export SECRET_GITHUB_API_TOKEN=[redacted]'
		);
	});

	test('redacts npm_-prefixed tokens', () => {
		assert.strictEqual(
			redact_secrets('using npm_abcdEFGH1234567890xyz to auth'),
			'using npm_abcd[redacted] to auth'
		);
	});

	test('leaves non-secret text untouched', () => {
		assert.strictEqual(redact_secrets('published pkg-a@1.0.1'), 'published pkg-a@1.0.1');
	});
});

describe('mask_secrets', () => {
	test('redacts string fields and preserves non-string fields', () => {
		const masked = mask_secrets(run_started);
		assert.strictEqual(masked.event, 'run_started');
		assert(masked.event === 'run_started');
		assert.strictEqual(masked.wetrun, false); // boolean preserved
		assert.strictEqual(masked.total, 3); // number preserved
	});
});

describe('summarize_events', () => {
	test('tallies outcomes and carries duration', () => {
		const events: Array<PublishingEvent> = [
			{ event: 'run_started', wetrun: false, total: 3 },
			completed,
			{ event: 'package_failed', name: 'b', error: 'boom', code: 'publish' },
			{ event: 'package_skipped', name: 'c', reason: 'no changesets' }
		];
		const summary = summarize_events(events, 1234);
		assert.strictEqual(summary.total, 3);
		assert.strictEqual(summary.published, 1);
		assert.strictEqual(summary.failed, 1);
		assert.strictEqual(summary.skipped, 1);
		assert.strictEqual(summary.duration, 1234);
	});
});
