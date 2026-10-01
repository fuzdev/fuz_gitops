import { assert, describe, test } from 'vitest';

import { format_and_output, output_is_machine, route_human_output } from '$lib/output_helpers.ts';
import { create_stream_log } from './test_helpers.ts';

describe('output_is_machine', () => {
	test('a json or markdown report on stdout is machine output', () => {
		assert.isTrue(output_is_machine('json', undefined));
		assert.isTrue(output_is_machine('markdown', undefined));
		assert.isFalse(output_is_machine('json', 'out.json'));
		assert.isFalse(output_is_machine('markdown', 'out.md'));
		assert.isFalse(output_is_machine('stdout', undefined));
		assert.isFalse(output_is_machine('text', undefined));
	});
});

describe('route_human_output', () => {
	test('machine: every level goes to stderr, children too, and the writer to stdout', () => {
		const log = create_stream_log();
		const write_stdout = route_human_output(log, true);
		log.info('info');
		log.debug('debug');
		log.warn('warn');
		log.error('error');
		log.child('child').info('child info');
		write_stdout('{"document":true}');
		assert.deepEqual(log.stdout, ['{"document":true}']);
		assert.strictEqual(log.stderr.length, 5);
	});

	test('human: the logger keeps its stdout', () => {
		const log = create_stream_log();
		const write_stdout = route_human_output(log, false);
		log.info('info');
		write_stdout('doc');
		assert.deepEqual(log.stdout, ['[test] info', 'doc']);
		assert.deepEqual(log.stderr, []);
	});
});

describe('format_and_output', () => {
	const formatters = {
		json: (data: { a: number }) => JSON.stringify(data),
		markdown: (data: { a: number }) => ['# doc', `a is ${data.a}`],
		stdout: () => {}
	};

	test('a json or markdown document goes through `write_stdout` in one write', async () => {
		const written: Array<string> = [];
		const write_stdout = (content: string) => written.push(content);
		await format_and_output({ a: 1 }, formatters, { format: 'json', write_stdout });
		await format_and_output({ a: 2 }, formatters, { format: 'markdown', write_stdout });
		assert.deepEqual(written, ['{"a":1}', '# doc\na is 2']);
	});
});
