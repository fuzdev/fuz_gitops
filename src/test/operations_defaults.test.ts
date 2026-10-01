import { assert, describe, test, vi, afterEach } from 'vitest';
import { spawn_out } from '@fuzdev/fuz_util/process.ts';

import {
	default_npm_operations,
	default_process_operations,
	output_tail,
	OUTPUT_TAIL_MAX_LINES
} from '$lib/operations_defaults.ts';

// `spawn_out` is mocked for `npm ping`; `run_interactive` spawns real `node` children
vi.mock('@fuzdev/fuz_util/process.ts', async (import_original) => {
	const actual = await import_original<typeof import('@fuzdev/fuz_util/process.ts')>();
	return { ...actual, spawn_out: vi.fn() };
});

afterEach(() => {
	vi.restoreAllMocks();
});

describe('output_tail', () => {
	test('keeps the last lines, trimming trailing whitespace', () => {
		const text = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join('\n') + '\n\n';
		const tail = output_tail(text);
		const lines = tail.split('\n');
		assert.strictEqual(lines.length, OUTPUT_TAIL_MAX_LINES);
		assert.strictEqual(lines[0], `line ${30 - OUTPUT_TAIL_MAX_LINES + 1}`);
		assert.strictEqual(lines.at(-1), 'line 30');
	});

	test('bounds the characters after the lines, dropping the line the cut splits', () => {
		assert.strictEqual(output_tail('a\nbbbbbbbbbb\ncc', 20, 6), 'cc');
		assert.strictEqual(output_tail('a\nbbbbbbbbbb\ncc', 20, 12), 'cc');
		// a cut on a line boundary keeps the whole first line
		assert.strictEqual(output_tail('a\nbbbbbbbbbb\ncc', 20, 13), 'bbbbbbbbbb\ncc');
		// exactly at the limit, nothing is cut
		assert.strictEqual(output_tail('a\nbbbbbbbbbb\ncc', 20, 15), 'a\nbbbbbbbbbb\ncc');
	});

	test('normalizes carriage returns and drops leading blank lines', () => {
		assert.strictEqual(output_tail('a\r\nb\rc\r\n'), 'a\nb\nc');
		assert.strictEqual(output_tail('\n\n  error'), '  error');
	});

	test('never returns part of a token the character cut straddles', () => {
		const token = 'npm_' + 'A'.repeat(36);
		const line = `npm error auth ${token} rejected`;
		// the cut lands inside the token, past its `npm_` prefix
		const max_chars = 'A'.repeat(20).length + ' rejected\nnpm error code E401'.length;
		const multi = output_tail(`${line}\nnpm error code E401`, 20, max_chars);
		assert.strictEqual(multi, 'npm error code E401');
		assert.notInclude(multi, 'AAAA');
		// a single line with nothing whole to keep yields nothing
		assert.strictEqual(output_tail(line, 20, 30), '');
	});

	test('strips terminal escape sequences', () => {
		assert.strictEqual(output_tail('\u001b[31merror\u001b[39m\n'), 'error');
	});

	test('is empty for empty output', () => {
		assert.strictEqual(output_tail(''), '');
		assert.strictEqual(output_tail('\n  \n'), '');
	});
});

describe('default_npm_operations.check_registry', () => {
	test('succeeds on exit 0 with nothing on stdout, as `npm ping` reports on stderr', async () => {
		vi.mocked(spawn_out).mockResolvedValue({
			result: { kind: 'exited', ok: true, code: 0 },
			stdout: '',
			stderr: 'npm notice PING https://registry.npmjs.org/\nnpm notice PONG 120ms\n'
		} as any);
		const result = await default_npm_operations.check_registry();
		assert.ok(result.ok);
		assert.deepEqual(vi.mocked(spawn_out).mock.calls[0]?.slice(0, 2), ['npm', ['ping']]);
	});

	test('fails on a non-zero exit, naming it', async () => {
		vi.mocked(spawn_out).mockResolvedValue({
			result: { kind: 'exited', ok: false, code: 1 },
			stdout: '',
			stderr: 'npm error network'
		} as any);
		const result = await default_npm_operations.check_registry();
		assert.ok(!result.ok);
		assert.include(result.message, 'code 1');
	});
});

describe('default_process_operations.run_interactive', () => {
	/** Runs `node -e <script>`, capturing what the op echoes to our stderr. */
	const run_node = async (
		script: string
	): Promise<{
		result: Awaited<ReturnType<typeof default_process_operations.run_interactive>>;
		echoed: string;
	}> => {
		let echoed = '';
		vi.spyOn(process.stderr, 'write').mockImplementation((chunk: string | Uint8Array) => {
			echoed += String(chunk);
			return true;
		});
		const result = await default_process_operations.run_interactive({
			cmd: process.execPath,
			args: ['-e', script]
		});
		vi.mocked(process.stderr.write).mockRestore();
		return { result, echoed };
	};

	test('succeeds on exit 0', async () => {
		const { result } = await run_node('process.exit(0)');
		assert.ok(result.ok);
	});

	test('a failure names the exit code and carries the end of stderr, echoed live', async () => {
		const { result, echoed } = await run_node(
			"process.stderr.write('first\\nnpm error code E401\\n'); process.exit(3)"
		);
		assert.ok(!result.ok);
		assert.include(result.message, 'code 3');
		assert.strictEqual(result.stderr_tail, 'first\nnpm error code E401');
		assert.strictEqual(echoed, 'first\nnpm error code E401\n');
	});

	test('a failure with nothing on stderr has no tail', async () => {
		const { result } = await run_node('process.exit(1)');
		assert.ok(!result.ok);
		assert.strictEqual(result.stderr_tail, undefined);
	});

	test('a command that does not exist fails with its reason', async () => {
		const result = await default_process_operations.run_interactive({
			cmd: 'fuz_gitops_no_such_command',
			args: []
		});
		assert.ok(!result.ok);
		assert.include(result.message, 'ENOENT');
	});
});
