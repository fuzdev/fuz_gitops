import type { LibraryJson } from '@fuzdev/fuz_util/library_json.ts';
import type { PackageJson } from '@fuzdev/fuz_util/package_json.ts';
import { Library } from '@fuzdev/fuz_ui/library.svelte.ts';

import type { LocalRepo } from '$lib/local_repo.ts';
import type {
	GitopsOperations,
	ChangesetOperations,
	GitOperations,
	FsOperations,
	NpmOperations,
	BuildOperations,
	ProcessOperations,
	ReposCommandOutput,
	ReposOperations
} from '$lib/operations.ts';
import type { BumpType } from '$lib/version_utils.ts';
import {
	REPOS_STATUS_FORMAT_VERSION,
	type ReposEntryStatus,
	type ReposStatusReport
} from '$lib/repos_status.ts';

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
		run_preflight_checks: async () => ({
			ok: true,
			warnings: [],
			errors: [],
			repos_with_changesets: new Set(),
			repos_without_changesets: new Set()
		}),
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
 * Creates mock ChangesetOperations with custom version predictions
 */
export const create_mock_changeset_ops = (
	versionPredictions: Map<string, { version: string; bump_type: BumpType }>,
	reposWithChangesets: Set<string> = new Set()
): ChangesetOperations => ({
	has_changesets: async (options) => ({
		ok: true,
		value: reposWithChangesets.has(options.repo.library.name)
	}),
	read_changesets: async () => ({ ok: true, value: [] }),
	predict_next_version: async (options) => {
		const prediction = versionPredictions.get(options.repo.library.name);
		if (!prediction) return null;
		return { ok: true, ...prediction };
	}
});

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
 * Creates a successful preflight mock with specified repos
 */
export const create_preflight_mock = (
	repos_with_changesets: Array<string> = [],
	repos_without_changesets: Array<string> = []
): {
	run_preflight_checks: () => Promise<{
		ok: boolean;
		warnings: Array<string>;
		errors: Array<string>;
		repos_with_changesets: Set<string>;
		repos_without_changesets: Set<string>;
	}>;
} => ({
	run_preflight_checks: async () => ({
		ok: true,
		warnings: [],
		errors: [],
		repos_with_changesets: new Set(repos_with_changesets),
		repos_without_changesets: new Set(repos_without_changesets)
	})
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
 * records each call. `entries` replaces the entry for a key.
 */
export const create_ready_repos_ops = (
	entries: Record<string, ReposEntryStatus> = {}
): ReposOperations & {
	calls: Array<{ keys: Array<string>; registry?: string; fetch?: boolean }>;
} => {
	const calls: Array<{ keys: Array<string>; registry?: string; fetch?: boolean }> = [];
	return {
		calls,
		status: async (options) => {
			calls.push(options);
			const report = create_mock_repos_report(
				options.keys.map((key) => entries[key] ?? create_mock_repos_entry({ key })),
				{ fetched: options.fetch === true }
			);
			return { ok: true, output: { stdout: JSON.stringify(report), stderr: '', exit_code: 0 } };
		}
	};
};
