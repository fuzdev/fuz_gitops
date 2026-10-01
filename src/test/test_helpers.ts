import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { LibraryJson } from '@fuzdev/fuz_util/library_json.ts';
import type { PackageJson } from '@fuzdev/fuz_util/package_json.ts';
import { Library } from '@fuzdev/fuz_ui/library.svelte.ts';
import { Logger } from '@fuzdev/fuz_util/log.ts';

import type { LocalRepo } from '$lib/local_repo.ts';
import type {
	GitopsOperations,
	GitOperations,
	FsOperations,
	NpmOperations,
	BuildOperations,
	ProcessOperations,
	ReposCommandOutput,
	ReposOperations
} from '$lib/operations.ts';
import type { RepoReadinessProblem } from '$lib/repo_readiness.ts';
import {
	REPOS_STATUS_FORMAT_VERSION,
	type ReposCheckout,
	type ReposEntryStatus,
	type ReposStatusReport
} from '$lib/repos_status.ts';

/** A logger that records what reaches each stream, as Node's console routes it. */
export type StreamLog = Logger & {
	stdout: Array<string>;
	stderr: Array<string>;
	/** Every line on either stream, in the order logged. */
	lines: Array<string>;
};

/**
 * Creates a logger at `debug` whose console records by stream the way Node's
 * does: `log` on `stdout`, `warn` and `error` on `stderr`, and every line in
 * order on `lines`. A task that routes its human output to stderr
 * (`route_human_output`) leaves `stdout` holding only its document.
 */
export const create_stream_log = (): StreamLog => {
	const stdout: Array<string> = [];
	const stderr: Array<string> = [];
	const lines: Array<string> = [];
	const record =
		(stream: Array<string>) =>
		(...args: Array<unknown>): void => {
			const line = args.join(' ');
			stream.push(line);
			lines.push(line);
		};
	const log = new Logger('test', {
		level: 'debug',
		colors: false,
		console: { log: record(stdout), warn: record(stderr), error: record(stderr) }
	});
	return Object.assign(log, { stdout, stderr, lines });
};

export interface MockRepoOptions {
	name: string;
	version?: string;
	deps?: Record<string, string>;
	dev_deps?: Record<string, string>;
	peer_deps?: Record<string, string>;
	private?: boolean;
	/** Defaults to `'npm'`; pass `'cargo'` to mock a dashboard-only non-npm repo. */
	kind?: 'npm' | 'cargo';
}

/**
 * Creates a mock full `package.json` for testing. Used for `LocalRepo.package_json`
 * and — mirroring production, where the loader feeds the full manifest down to the
 * curated slot — for `LibraryJson.pkg_json` too.
 */
export const create_mock_package_json = (options: MockRepoOptions): PackageJson => {
	const {
		name,
		version = '1.0.0',
		deps = {},
		dev_deps = {},
		peer_deps = {},
		private: private_option = false
	} = options;
	return {
		name,
		version,
		private: private_option,
		repository: { type: 'git', url: `git+https://github.com/test/${name}.git` },
		dependencies: Object.keys(deps).length > 0 ? deps : undefined,
		devDependencies: Object.keys(dev_deps).length > 0 ? dev_deps : undefined,
		peerDependencies: Object.keys(peer_deps).length > 0 ? peer_deps : undefined
	};
};

/**
 * Creates a mock LibraryJson for testing — the raw `pkg_json`/`source_json` pair.
 */
export const create_mock_library_json = (options: MockRepoOptions): LibraryJson => ({
	pkg_json: create_mock_package_json(options),
	source_json: { modules: [] }
});

/**
 * Creates a mock LocalRepo for testing
 */
export const create_mock_repo = (options: MockRepoOptions): LocalRepo => {
	const { name, deps = {}, dev_deps = {}, peer_deps = {}, kind = 'npm' } = options;
	const library_json = create_mock_library_json(options);

	return {
		kind,
		library: new Library(library_json),
		package_json: create_mock_package_json(options),
		repo_dir: `/test/${name}`,
		entry: create_mock_repos_entry({ key: name }),
		dependencies: new Map(Object.entries(deps)),
		dev_dependencies: new Map(Object.entries(dev_deps)),
		peer_dependencies: new Map(Object.entries(peer_deps))
	};
};

/**
 * Creates mock GitopsOperations with sensible defaults
 */
export const create_mock_gitops_ops = (
	overrides: Partial<{
		changeset: Partial<GitopsOperations['changeset']>;
		git: Partial<GitopsOperations['git']>;
		process: Partial<GitopsOperations['process']>;
		npm: Partial<GitopsOperations['npm']>;
		preflight: Partial<GitopsOperations['preflight']>;
		fs: Partial<GitopsOperations['fs']>;
		build: Partial<GitopsOperations['build']>;
		repos: Partial<GitopsOperations['repos']>;
	}> = {}
): GitopsOperations => ({
	changeset: {
		has_changesets: async () => ({ ok: true, value: true }),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => ({
			ok: true,
			version: incrementPatch(options.repo.package_json.version || '0.0.0'),
			bump_type: 'patch' as const
		}),
		...overrides.changeset
	},
	git: create_mock_git_ops(overrides.git),
	process: {
		run_interactive: async () => ({ ok: true }),
		...overrides.process
	},
	npm: create_mock_npm_ops(overrides.npm),
	preflight: {
		run_preflight_checks: async () => ({ ok: true, warnings: [], errors: [] }),
		...overrides.preflight
	},
	fs: {
		readFile: async () => ({ ok: true, value: '{}' }),
		writeFile: async () => ({ ok: true }),
		mkdir: async () => ({ ok: true }),
		exists: async () => true,
		...overrides.fs
	},
	build: create_mock_build_ops(overrides.build),
	repos: { ...create_ready_repos_ops(), ...overrides.repos }
});

/**
 * Helper to increment patch version
 */
const incrementPatch = (version: string): string => {
	const [major, minor, patch] = version.split('.').map(Number);
	return `${major!}.${minor!}.${patch! + 1}`;
};

/**
 * Creates a map of package.json file paths to contents for testing
 */
export const create_mock_package_json_files = (
	repos: Array<LocalRepo>,
	updatedVersions: Map<string, string> = new Map()
): Map<string, string> => {
	const fs: Map<string, string> = new Map();

	for (const repo of repos) {
		const version =
			updatedVersions.get(repo.library.name) ||
			incrementPatch(repo.package_json.version || '0.0.0');

		const packageJson = {
			...repo.package_json,
			version
		};

		fs.set(`${repo.repo_dir}/package.json`, JSON.stringify(packageJson, null, 2));
	}

	return fs;
};

/**
 * Creates mock GitOperations for testing
 */
export const create_mock_git_ops = (overrides: Partial<GitOperations> = {}): GitOperations => ({
	current_commit_hash: async () => ({ ok: true, value: 'abc123' }),
	add: async () => ({ ok: true }),
	commit: async () => ({ ok: true }),
	...overrides
});

/**
 * Creates mock NpmOperations for testing
 */
export const create_mock_npm_ops = (overrides: Partial<NpmOperations> = {}): NpmOperations => ({
	wait_for_package: async () => ({ ok: true }),
	check_auth: async () => ({ ok: true, username: 'testuser' }),
	check_registry: async () => ({ ok: true }),
	...overrides
});

/**
 * Creates mock BuildOperations for testing
 */
export const create_mock_build_ops = (
	overrides: Partial<BuildOperations> = {}
): BuildOperations => ({
	build_package: async () => ({ ok: true }),
	...overrides
});

/**
 * Creates mock FsOperations for testing with in-memory storage
 */
export const create_mock_fs_ops = (): FsOperations & {
	get: (path: string) => string | undefined;
	set: (path: string, content: string) => void;
} => {
	const files: Map<string, string> = new Map();
	const dirs: Set<string> = new Set();

	return {
		readFile: async (options) => {
			const content = files.get(options.path);
			if (content === undefined) {
				return { ok: false, kind: 'not_found', message: `File not found: ${options.path}` };
			}
			return { ok: true, value: content };
		},
		writeFile: async (options) => {
			files.set(options.path, options.content);
			return { ok: true };
		},
		mkdir: async (options) => {
			dirs.add(options.path);
			return { ok: true };
		},
		exists: async (options) => {
			return files.has(options.path) || dirs.has(options.path);
		},
		get: (path: string): string | undefined => files.get(path),
		set: (path: string, content: string): void => {
			files.set(path, content);
		}
	};
};

/**
 * Creates and populates fs ops from package.json files
 */
export const create_populated_fs_ops = (
	repos: Array<LocalRepo>,
	updated_versions?: Map<string, string>
): FsOperations & {
	get: (path: string) => string | undefined;
	set: (path: string, content: string) => void;
} => {
	const fs_ops = create_mock_fs_ops();
	const package_files = create_mock_package_json_files(repos, updated_versions);
	for (const [path, content] of package_files) {
		fs_ops.set(path, content);
	}
	return fs_ops;
};

/**
 * Tracked command for process operations
 */
export interface TrackedCommand {
	cmd: string;
	args: Array<string>;
	cwd: string;
	/** Where the command's stdout was routed, as the executor asked. */
	stdout?: 'stdout' | 'stderr';
}

/**
 * Creates process operations that track which commands were run
 */
export const create_tracking_process_ops = (): {
	ops: ProcessOperations;
	get_spawned_commands: () => Array<TrackedCommand>;
	get_commands_by_type: (cmd_name: string) => Array<TrackedCommand>;
	get_package_names_from_cwd: (commands: Array<TrackedCommand>) => Array<string>;
} => {
	const spawned_commands: Array<TrackedCommand> = [];

	return {
		ops: {
			run_interactive: async (options) => {
				spawned_commands.push({
					cmd: options.cmd,
					args: options.args,
					cwd: options.cwd ?? '',
					stdout: options.stdout
				});
				return { ok: true };
			}
		},
		get_spawned_commands: () => spawned_commands,
		get_commands_by_type: (cmd_name: string) =>
			spawned_commands.filter((c) => c.cmd === 'gro' && c.args[0] === cmd_name),
		get_package_names_from_cwd: (commands: Array<TrackedCommand>) =>
			commands.map((c) => c.cwd.split('/').pop() || '')
	};
};

/**
 * Creates a mock `repos status` entry: an owned public repo, present, clean,
 * and on its branch `main`, in sync with origin. `overrides` replace fields whole.
 */
export const create_mock_repos_entry = (
	overrides: Partial<ReposEntryStatus> & { key: string }
): ReposEntryStatus => {
	const { key } = overrides;
	const dir = overrides.dir ?? key;
	return {
		kind: 'repo',
		dir,
		url: `https://github.com/test/${key}`,
		writable: true,
		archived: false,
		visibility: 'public',
		ci: true,
		branch: 'main',
		pinned: false,
		refresh: null,
		presence: { kind: 'present' },
		clone: null,
		layout: { shallow: false, sparse: false, partial_filter: null },
		checkouts: [
			{
				path: `/test/${dir}`,
				primary: true,
				head: { kind: 'branch', name: 'main' },
				uncommitted: { staged: 0, unstaged: 0, untracked: 0, conflicted: 0 },
				in_progress: null,
				locked: false,
				linked: false,
				submodules: null,
				busy: []
			}
		],
		branches: [],
		at_rest: { on_branch: true, clean: true, idle: true, followed: { kind: 'in_sync' } },
		stashes: 0,
		fetched_at: null,
		needs_human: [],
		probe_error: null,
		unprobed_worktrees: [],
		fetch_error: null,
		visibility_check: null,
		...overrides
	};
};

/**
 * Creates a mock `repos status --json` report over `entries`, its workspace `/test`.
 */
export const create_mock_repos_report = (
	entries: Array<ReposEntryStatus>,
	overrides: Partial<ReposStatusReport> = {}
): ReposStatusReport => ({
	version: REPOS_STATUS_FORMAT_VERSION,
	workspace: '/test',
	registry: '/test/repos.toml',
	fetched: false,
	sessions: { kind: 'available', unscoped: [] },
	entries,
	unregistered: null,
	...overrides
});

/**
 * Creates mock ReposOperations whose `status` prints `printed` (a document as
 * JSON, or raw text) and records each call's options. It exits `2` for an
 * error document, else `0`.
 */
export const create_mock_repos_ops = (
	printed: object | string,
	overrides: Partial<ReposOperations> = {}
): ReposOperations & {
	calls: Array<{ keys: Array<string>; registry?: string; fetch?: boolean }>;
} => {
	const calls: Array<{ keys: Array<string>; registry?: string; fetch?: boolean }> = [];
	const stdout = typeof printed === 'string' ? printed : JSON.stringify(printed);
	const output: ReposCommandOutput = {
		stdout,
		stderr: '',
		exit_code: typeof printed === 'object' && 'error' in printed ? 2 : 0
	};
	return {
		calls,
		status: async (options) => {
			calls.push(options);
			return { ok: true, output };
		},
		...overrides
	};
};

/**
 * Creates mock ReposOperations whose `status` reports every requested key as a
 * ready entry (`create_mock_repos_entry`), fetched when asked to fetch, and
 * records each call. `entries` replaces the entry for a key; `fetched`, when
 * given, replaces it instead in a run with `--fetch`.
 */
export const create_ready_repos_ops = (
	entries: Record<string, ReposEntryStatus> = {},
	fetched: Record<string, ReposEntryStatus> = entries
): ReposOperations & {
	calls: Array<{ keys: Array<string>; registry?: string; fetch?: boolean }>;
} => {
	const calls: Array<{ keys: Array<string>; registry?: string; fetch?: boolean }> = [];
	return {
		calls,
		status: async (options) => {
			calls.push(options);
			const by_key = options.fetch ? fetched : entries;
			const report = create_mock_repos_report(
				options.keys.map((key) => by_key[key] ?? create_mock_repos_entry({ key })),
				{ fetched: options.fetch === true }
			);
			return { ok: true, output: { stdout: JSON.stringify(report), stderr: '', exit_code: 0 } };
		}
	};
};

/**
 * Two npm repos, `b` depending on `a`, and a cargo one `c`, for the readiness
 * gate's tests; `b_entry` is `b`'s entry as the local, unfetched status read it.
 */
export const create_gate_repos = (b_entry?: ReposEntryStatus): Array<LocalRepo> => {
	const a = create_mock_repo({ name: 'a' });
	const b = create_mock_repo({ name: 'b', deps: { a: '^1.0.0' } });
	if (b_entry) b.entry = b_entry;
	const c = create_mock_repo({ name: 'c', kind: 'cargo' });
	return [a, b, c];
};

/**
 * The `repos --json` golden documents, written by the Rust side
 * (`crates/fuz_repos/tests/golden.rs`), never by hand.
 */
export const GOLDEN_DIR = join(dirname(fileURLToPath(import.meta.url)), 'fixtures/repos_status');

/** Reads the golden `name` as the text `repos` printed. */
export const read_golden = (name: string): string => readFileSync(join(GOLDEN_DIR, name), 'utf8');

/** Reads the golden `name` as parsed JSON. */
export const load_golden = (name: string): unknown => JSON.parse(read_golden(name));

/** An entry `key` whose primary checkout overrides `checkout`, with `at_rest` set whole. */
export const entry_with = (
	key: string,
	options: {
		checkout?: Partial<ReposCheckout>;
		at_rest?: ReposEntryStatus['at_rest'];
	} & Partial<Omit<ReposEntryStatus, 'at_rest' | 'checkouts'>>
): ReposEntryStatus => {
	const { checkout, at_rest, ...rest } = options;
	const base = create_mock_repos_entry({ key });
	const primary = { ...base.checkouts[0]!, path: `/test/${key}`, ...checkout };
	return {
		...base,
		...rest,
		checkouts: [primary],
		at_rest: at_rest === undefined ? base.at_rest : at_rest
	};
};

export const AT_REST = {
	on_branch: true,
	clean: true,
	idle: true,
	followed: { kind: 'in_sync' }
} as const;

export const kinds = (problems: Array<RepoReadinessProblem>): Array<string> =>
	problems.map((p) => p.kind);
