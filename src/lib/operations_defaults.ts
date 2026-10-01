/**
 * Production implementations of operations interfaces.
 *
 * Provides real git, npm, fs, build, and `repos` operations for production use.
 * For interface definitions and dependency injection pattern, see `operations.ts`.
 *
 * @module
 */

import { spawn_out, spawn_process, spawn_result_to_message } from '@fuzdev/fuz_util/process.ts';
import { readFile, writeFile, mkdir, stat } from 'node:fs/promises';
import { stripVTControlCharacters } from 'node:util';
import { fs_classify_error } from '@fuzdev/fuz_util/fs.ts';
import { EMPTY_OBJECT } from '@fuzdev/fuz_util/object.ts';

import { has_changesets, read_changesets, predict_next_version } from './changeset_reader.ts';
import { wait_for_package } from './npm_registry.ts';
import { run_preflight_checks } from './preflight_checks.ts';
import { git_add, git_commit, git_current_commit_hash_required } from './git_operations.ts';
import type {
	ChangesetOperations,
	GitOperations,
	ProcessOperations,
	NpmOperations,
	PreflightOperations,
	FsOperations,
	BuildOperations,
	GitopsOperations,
	ReposOperations
} from './operations.ts';

/** Wrap an async function that returns a value */
const wrap_with_value = async <T>(
	fn: () => Promise<T>
): Promise<{ ok: true; value: T } | { ok: false; message: string }> => {
	try {
		const value = await fn();
		return { ok: true, value };
	} catch (error) {
		return { ok: false, message: String(error) };
	}
};

/** Wrap an async function, ignoring its return value */
const wrap_void = async (
	fn: () => Promise<unknown>
): Promise<{ ok: true } | { ok: false; message: string }> => {
	try {
		await fn();
		return { ok: true };
	} catch (error) {
		return { ok: false, message: String(error) };
	}
};

export const default_changeset_operations: ChangesetOperations = {
	has_changesets: async (options) => {
		const { repo } = options;
		return wrap_with_value(() => has_changesets(repo));
	},

	read_changesets: async (options) => {
		const { repo, log } = options;
		return wrap_with_value(() => read_changesets(repo, log));
	},

	predict_next_version: async (options) => {
		const { repo, log } = options;
		try {
			const result = await predict_next_version(repo, log);
			if (result === null) {
				return null;
			}
			return { ok: true, ...result };
		} catch (error) {
			return { ok: false, message: String(error) };
		}
	}
};

export const default_git_operations: GitOperations = {
	current_commit_hash: async (options) => {
		const { cwd } = options ?? EMPTY_OBJECT;
		return wrap_with_value(() => git_current_commit_hash_required(cwd ? { cwd } : undefined));
	},

	add: async (options) => {
		const { files, cwd } = options;
		return wrap_void(() => git_add(files, cwd ? { cwd } : undefined));
	},

	commit: async (options) => {
		const { message, files, cwd } = options;
		return wrap_void(() => git_commit(message, files, cwd ? { cwd } : undefined));
	}
};

/** The most lines of a child's stderr a failure carries. */
export const OUTPUT_TAIL_MAX_LINES = 20;

/** The most characters of a child's stderr a failure carries. */
export const OUTPUT_TAIL_MAX_CHARS = 4096;

/**
 * The end of a process's output, for a failure message: its last lines, then
 * its last characters, with terminal escape sequences stripped and trailing
 * whitespace trimmed.
 *
 * @param text - the output, or as much of its end as was kept
 * @param max_lines - the most lines to keep
 * @param max_chars - the most characters to keep, applied after `max_lines`
 */
export const output_tail = (
	text: string,
	max_lines: number = OUTPUT_TAIL_MAX_LINES,
	max_chars: number = OUTPUT_TAIL_MAX_CHARS
): string => {
	const lines = stripVTControlCharacters(text).replace(/\r\n?/g, '\n').trimEnd().split('\n');
	const tail = lines.slice(-max_lines).join('\n');
	if (tail.length <= max_chars) return drop_leading_blank_lines(tail);
	// a character cut can split a line, and with it a secret masking would no longer
	// recognize, so the partial first line goes
	const cut = tail.slice(-max_chars);
	if (tail[tail.length - max_chars - 1] === '\n') return drop_leading_blank_lines(cut);
	const nl = cut.indexOf('\n');
	return nl === -1 ? '' : drop_leading_blank_lines(cut.slice(nl + 1));
};

// not `trimStart`, which would strip the first line's indentation
const drop_leading_blank_lines = (text: string): string => text.replace(/^\n+/, '');

export const default_process_operations: ProcessOperations = {
	run_interactive: async (options) => {
		const { cmd, args, cwd, stdout = 'stdout' } = options;
		try {
			// stdin and stdout go straight to the child (fd 2 for stdout routed to our stderr), so
			// they stay a TTY when ours are — npm prompts for a one-time password only on a TTY.
			// Only stderr is piped, to keep its end for the failure message while echoing it live.
			const { child, closed } = spawn_process(cmd, args, {
				cwd,
				stdio: ['inherit', stdout === 'stderr' ? 2 : 'inherit', 'pipe']
			});
			// a rolling window over stderr's end, bounded however much the child writes
			let kept = '';
			child.stderr?.setEncoding('utf8');
			child.stderr?.on('data', (chunk: string) => {
				process.stderr.write(chunk);
				kept += chunk;
				if (kept.length > OUTPUT_TAIL_MAX_CHARS * 4) kept = kept.slice(-OUTPUT_TAIL_MAX_CHARS * 2);
			});
			const result = await closed;
			if (result.ok) return { ok: true };
			const stderr_tail = output_tail(kept);
			return {
				ok: false,
				message: `\`${[cmd, ...args].join(' ')}\` failed (${spawn_result_to_message(result)})`,
				stderr_tail: stderr_tail || undefined
			};
		} catch (error) {
			return { ok: false, message: String(error) };
		}
	}
};

export const default_repos_operations: ReposOperations = {
	status: async (options) => {
		const { keys, registry, fetch } = options;
		const args = [...(registry === undefined ? [] : ['--registry', registry]), 'status'];
		if (fetch) args.push('--fetch');
		// `--` so a key can never read as a flag
		args.push('--json', '--', ...keys);
		const spawned = await spawn_out('repos', args);
		const { result } = spawned;
		if (result.kind === 'error') {
			const not_found = (result.error as NodeJS.ErrnoException).code === 'ENOENT';
			return {
				ok: false,
				kind: not_found ? 'not_found' : 'failed',
				message: result.error.message
			};
		}
		if (result.kind === 'signaled') {
			return { ok: false, kind: 'failed', message: `repos was killed by ${result.signal}` };
		}
		return {
			ok: true,
			output: {
				stdout: spawned.stdout ?? '',
				stderr: spawned.stderr ?? '',
				exit_code: result.code
			}
		};
	}
};

export const default_npm_operations: NpmOperations = {
	wait_for_package: async (options) => {
		const { pkg, version, wait_options, log } = options;
		try {
			await wait_for_package(pkg, version, { ...wait_options, log });
			return { ok: true };
		} catch (error) {
			return { ok: false, message: String(error) };
		}
	},

	check_auth: async () => {
		try {
			const result = await spawn_out('npm', ['whoami']);
			if (result.stdout) {
				const username = result.stdout.trim();
				if (username) {
					return { ok: true, username };
				}
			}
			return { ok: false, message: 'Not logged in to npm' };
		} catch (error) {
			return { ok: false, message: String(error) };
		}
	},

	check_registry: async () => {
		try {
			// the exit status alone: `npm ping` reports on stderr, writing nothing to stdout
			const { result } = await spawn_out('npm', ['ping']);
			if (result.ok) {
				return { ok: true };
			}
			return {
				ok: false,
				message: `Failed to ping npm registry (${spawn_result_to_message(result)})`
			};
		} catch (error) {
			return { ok: false, message: String(error) };
		}
	}
};

export const default_preflight_operations: PreflightOperations = {
	run_preflight_checks: async (options) => {
		return run_preflight_checks(options);
	}
};

export const default_fs_operations: FsOperations = {
	readFile: async (options) => {
		const { path, encoding } = options;
		try {
			const value = await readFile(path, encoding);
			return { ok: true, value };
		} catch (error) {
			return { ok: false, ...fs_classify_error(error) };
		}
	},

	writeFile: async (options) => {
		const { path, content } = options;
		try {
			await writeFile(path, content);
			return { ok: true };
		} catch (error) {
			return { ok: false, ...fs_classify_error(error) };
		}
	},

	mkdir: async (options) => {
		const { path, recursive } = options;
		try {
			await mkdir(path, { recursive });
			return { ok: true };
		} catch (error) {
			return { ok: false, ...fs_classify_error(error) };
		}
	},

	exists: async (options) => {
		try {
			await stat(options.path);
			return true;
		} catch {
			return false;
		}
	}
};

export const default_build_operations: BuildOperations = {
	build_package: async (options) => {
		const { repo } = options;
		try {
			const spawned = await spawn_out('gro', ['build'], { cwd: repo.repo_dir });
			if (spawned.result.ok) {
				return { ok: true };
			} else {
				return {
					ok: false,
					message: 'Build failed',
					output: spawned.stderr || spawned.stdout || 'Build failed'
				};
			}
		} catch (error) {
			return { ok: false, message: String(error) };
		}
	}
};

/**
 * Combined default operations for all gitops functionality.
 */
export const default_gitops_operations: GitopsOperations = {
	changeset: default_changeset_operations,
	git: default_git_operations,
	process: default_process_operations,
	npm: default_npm_operations,
	preflight: default_preflight_operations,
	fs: default_fs_operations,
	build: default_build_operations,
	repos: default_repos_operations
};
