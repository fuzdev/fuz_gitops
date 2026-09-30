/**
 * Production implementations of operations interfaces.
 *
 * Provides real git, npm, fs, build, and `repos` operations for production use.
 * For interface definitions and dependency injection pattern, see `operations.ts`.
 *
 * @module
 */

import { spawn_out } from '@fuzdev/fuz_util/process.ts';
import { readFile, writeFile, mkdir, stat } from 'node:fs/promises';
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
		const { branch, cwd } = options ?? EMPTY_OBJECT;
		return wrap_with_value(() =>
			git_current_commit_hash_required(branch, cwd ? { cwd } : undefined)
		);
	},

	add: async (options) => {
		const { files, cwd } = options;
		return wrap_void(() => git_add(files, cwd ? { cwd } : undefined));
	},

	commit: async (options) => {
		const { message, cwd } = options;
		return wrap_void(() => git_commit(message, cwd ? { cwd } : undefined));
	}
};

export const default_process_operations: ProcessOperations = {
	spawn: async (options) => {
		const { cmd, args, cwd } = options;
		try {
			const spawned = await spawn_out(cmd, args, cwd ? { cwd } : undefined);
			if (spawned.result.ok) {
				return {
					ok: true,
					stdout: spawned.stdout || undefined,
					stderr: spawned.stderr || undefined
				};
			} else {
				return {
					ok: false,
					message: 'Command failed',
					stderr: spawned.stderr || undefined
				};
			}
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
			return { ok: false, message: String(error), timeout: true };
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
			const result = await spawn_out('npm', ['ping']);
			if (result.stdout) {
				return { ok: true };
			}
			return { ok: false, message: 'Failed to ping npm registry' };
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
		const { repo, log } = options;
		try {
			log?.info(`  Building ${repo.library.name}...`);
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
