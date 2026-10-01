import { assert, describe, test } from 'vitest';
import { readdirSync } from 'node:fs';

import {
	REPOS_INSTALL_COMMAND,
	load_repos_status,
	parse_repos_status_output,
	to_repos_command
} from '$lib/repos_status_load.ts';
import { REPOS_STATUS_FORMAT_VERSION, ReposStatusErrorReport } from '$lib/repos_status.ts';
import { GOLDEN_DIR, create_mock_repos_ops, load_golden, read_golden } from './test_helpers.ts';

const ERROR_GOLDENS = readdirSync(GOLDEN_DIR)
	.filter((f) => /^error_report_[a-z_]+\.json$/.test(f))
	.sort();

/** Parses `stdout` as `repos status --json` printed it, returning the failure message. */
const failure_of = (stdout: string, exit_code = 2, stderr = ''): string => {
	const result = parse_repos_status_output({ stdout, stderr, exit_code });
	assert.ok(!result.ok, 'expected the output to be refused');
	return result.message;
};

/** A golden with its `version` replaced. */
const with_version = (name: string, version: unknown): string =>
	JSON.stringify({ ...JSON.parse(read_golden(name)), version });

describe('parse_repos_status_output', () => {
	test.each(['status_report.json', 'status_report_targeted.json'])('parses %s', (name) => {
		const result = parse_repos_status_output({
			stdout: read_golden(name),
			stderr: '',
			exit_code: 0
		});
		assert.ok(result.ok, result.ok ? '' : result.message);
		assert.strictEqual(result.report.version, REPOS_STATUS_FORMAT_VERSION);
		assert.ok(result.report.entries.length > 0);
	});

	describe('a format version mismatch names both versions and the install', () => {
		test('an older binary', () => {
			const message = failure_of(
				with_version('status_report.json', REPOS_STATUS_FORMAT_VERSION - 1),
				0
			);
			assert.include(message, `format ${REPOS_STATUS_FORMAT_VERSION - 1}`);
			assert.include(message, `format ${REPOS_STATUS_FORMAT_VERSION}`);
			assert.include(message, REPOS_INSTALL_COMMAND);
			assert.include(message, 'installed separately');
		});

		test('a newer binary', () => {
			const message = failure_of(
				with_version('status_report.json', REPOS_STATUS_FORMAT_VERSION + 1),
				0
			);
			assert.include(message, `format ${REPOS_STATUS_FORMAT_VERSION + 1}`);
			assert.include(message, 'upgrade @fuzdev/fuz_gitops');
		});

		test('checked before the error document is parsed', () => {
			const message = failure_of(
				with_version('error_report_unknown_entry.json', REPOS_STATUS_FORMAT_VERSION - 1)
			);
			assert.include(message, REPOS_INSTALL_COMMAND);
			assert.notInclude(message, '`mta`');
		});

		test('no version at all', () => {
			assert.include(failure_of(JSON.stringify({ entries: [] }), 0), 'status format null');
		});
	});

	describe('error documents', () => {
		test.each(ERROR_GOLDENS)('%s maps to a message', (name) => {
			const { error } = ReposStatusErrorReport.parse(load_golden(name));
			const message = failure_of(read_golden(name));
			switch (error.kind) {
				case 'unknown_entry':
					assert.include(message, `\`${error.name}\``);
					for (const suggestion of error.suggestions) {
						assert.include(message, `\`${suggestion}\``);
					}
					break;
				case 'registry_not_found':
					assert.include(message, error.message);
					assert.include(message, '--registry <path>');
					break;
				default:
					assert.include(message, error.kind);
					assert.include(message, error.message);
					if (error.hint) assert.include(message, error.hint);
			}
		});

		test('an unknown key with no suggestions', () => {
			const doc = JSON.parse(read_golden('error_report_unknown_entry.json'));
			doc.error.suggestions = [];
			const message = failure_of(JSON.stringify(doc));
			assert.include(message, "the registry (repos.toml) doesn't name");
			assert.notInclude(message, 'did you mean');
		});

		test('an error kind the mirror does not know', () => {
			const doc = JSON.parse(read_golden('error_report_io.json'));
			doc.error.kind = 'no_checkout';
			assert.include(failure_of(JSON.stringify(doc), 1), "error document this package can't parse");
		});
	});

	test('a report that drifted from the mirror', () => {
		const doc = JSON.parse(read_golden('status_report.json'));
		doc.surprise = true;
		assert.include(failure_of(JSON.stringify(doc), 0), "report this package can't parse");
	});

	test('no JSON on a fatal I/O error names stderr and the exit code', () => {
		const message = failure_of('', 1, 'error: failed to list the workspace root\n');
		assert.include(message, 'exited 1');
		assert.include(message, 'failed to list the workspace root');
	});

	test('JSON that is not a document', () => {
		assert.include(failure_of('[1, 2]', 0), 'not a document');
	});
});

describe('load_repos_status', () => {
	test('runs `repos status` for the keys and registry, returning its report', async () => {
		const repos_ops = create_mock_repos_ops(read_golden('status_report.json'));
		const result = await load_repos_status({
			keys: ['app', 'gro'],
			registry: '../repos.toml',
			repos_ops
		});
		assert.ok(result.ok);
		assert.deepEqual(repos_ops.calls, [{ keys: ['app', 'gro'], registry: '../repos.toml' }]);
	});

	test('refuses no keys without running, since no targets means every entry', async () => {
		const repos_ops = create_mock_repos_ops(read_golden('status_report.json'));
		const result = await load_repos_status({ keys: [], repos_ops });
		assert.ok(!result.ok);
		assert.deepEqual(repos_ops.calls, []);
	});

	test('a binary not on PATH names the install command', async () => {
		const repos_ops = create_mock_repos_ops('', {
			status: async () => ({ ok: false, kind: 'not_found', message: 'spawn repos ENOENT' })
		});
		const result = await load_repos_status({ keys: ['gro'], repos_ops });
		assert.ok(!result.ok);
		assert.include(result.message, 'not found on PATH');
		assert.include(result.message, REPOS_INSTALL_COMMAND);
	});

	test('a binary that did not run to an exit', async () => {
		const repos_ops = create_mock_repos_ops('', {
			status: async () => ({ ok: false, kind: 'failed', message: 'repos was killed by SIGKILL' })
		});
		const result = await load_repos_status({ keys: ['gro'], repos_ops });
		assert.ok(!result.ok);
		assert.include(result.message, 'SIGKILL');
	});
});

describe('to_repos_command', () => {
	test('no registry is plain `repos`', () => {
		assert.strictEqual(to_repos_command(undefined), 'repos');
	});

	test('a plain path passes unquoted', () => {
		assert.strictEqual(to_repos_command('../repos.toml'), 'repos --registry ../repos.toml');
	});

	test('a path with spaces or quotes is POSIX-shell-quoted', () => {
		assert.strictEqual(to_repos_command("/a b/it's.toml"), `repos --registry '/a b/it'\\''s.toml'`);
	});
});
