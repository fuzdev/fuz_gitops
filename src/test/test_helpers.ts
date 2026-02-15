import type {Logger} from '@fuzdev/fuz_util/log.js';
import {vi} from 'vitest';
import type {LibraryJson} from '@fuzdev/fuz_util/library_json.js';
import {Library} from '@fuzdev/fuz_ui/library.svelte.js';

import type {LocalRepo} from '$lib/local_repo.js';
import type {
	GitopsOperations,
	ChangesetOperations,
	GitOperations,
	FsOperations,
	NpmOperations,
	BuildOperations,
	ProcessOperations,
} from '$lib/operations.js';
import type {BumpType} from '$lib/semver.js';

/* eslint-disable @typescript-eslint/require-await */

export interface MockRepoOptions {
	name: string;
	version?: string;
	deps?: Record<string, string>;
	dev_deps?: Record<string, string>;
	peer_deps?: Record<string, string>;
	private?: boolean;
}

/**
 * Creates a mock LibraryJson for testing
 */
export const create_mock_library_json = (options: MockRepoOptions): LibraryJson => {
	const {
		name,
		version = '1.0.0',
		deps = {},
		dev_deps = {},
		peer_deps = {},
		private: private_option = false,
	} = options;

	return {
		name,
		repo_name: name,
		repo_url: `https://github.com/test/${name}`,
		owner_name: 'test',
		homepage_url: `https://test.com/${name}`,
		logo_url: null,
		logo_alt: `logo for ${name}`,
		npm_url: null,
		changelog_url: null,
		published: false,
		package_json: {
			name,
			version,
			private: private_option,
			dependencies: Object.keys(deps).length > 0 ? deps : undefined,
			devDependencies: Object.keys(dev_deps).length > 0 ? dev_deps : undefined,
			peerDependencies: Object.keys(peer_deps).length > 0 ? peer_deps : undefined,
			repository: {type: 'git', url: `git+https://github.com/test/${name}.git`},
		},
		source_json: {name, version, modules: []},
	};
};

/**
 * Creates a mock LocalRepo for testing
 */
export const create_mock_repo = (options: MockRepoOptions): LocalRepo => {
	const {name, deps = {}, dev_deps = {}, peer_deps = {}} = options;
	const library_json = create_mock_library_json(options);

	return {
		library: new Library(library_json),
		library_json,
		repo_dir: `/test/${name}`,
		repo_git_ssh_url: `git@github.com:test/${name}.git`,
		repo_config: {
			repo_url: `https://github.com/test/${name}`,
			repo_dir: null,
			branch: 'main',
		},
		dependencies: new Map(Object.entries(deps)),
		dev_dependencies: new Map(Object.entries(dev_deps)),
		peer_dependencies: new Map(Object.entries(peer_deps)),
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
	}> = {},
): GitopsOperations => ({
	changeset: {
		has_changesets: async () => ({ok: true, value: true}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async (options) => ({
			ok: true,
			version: incrementPatch(options.repo.library.package_json.version || '0.0.0'),
			bump_type: 'patch' as const,
		}),
		...overrides.changeset,
	},
	git: create_mock_git_ops(overrides.git),
	process: {
		spawn: async () => ({ok: true}),
		...overrides.process,
	},
	npm: create_mock_npm_ops(overrides.npm),
	preflight: {
		run_preflight_checks: async () => ({
			ok: true,
			warnings: [],
			errors: [],
			repos_with_changesets: new Set(),
			repos_without_changesets: new Set(),
		}),
		...overrides.preflight,
	},
	fs: {
		readFile: async () => ({ok: true, value: '{}'}),
		writeFile: async () => ({ok: true}),
		mkdir: async () => ({ok: true}),
		exists: () => true,
		...overrides.fs,
	},
	build: create_mock_build_ops(overrides.build),
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
	updatedVersions: Map<string, string> = new Map(),
): Map<string, string> => {
	const fs: Map<string, string> = new Map();

	for (const repo of repos) {
		const version =
			updatedVersions.get(repo.library.name) ||
			incrementPatch(repo.library.package_json.version || '0.0.0');

		const packageJson = {
			...repo.library.package_json,
			version,
		};

		fs.set(`${repo.repo_dir}/package.json`, JSON.stringify(packageJson, null, 2));
	}

	return fs;
};

/**
 * Creates a mock repo with simulated changesets directory
 */
export const create_mock_repo_with_changesets = (
	options: MockRepoOptions & {changesets?: boolean},
): LocalRepo & {has_changesets: boolean} => {
	const repo = create_mock_repo(options);
	const has_changesets = options.changesets ?? true;

	return {
		...repo,
		has_changesets,
	};
};

/**
 * Creates mock ChangesetOperations with custom version predictions
 */
export const create_mock_changeset_ops = (
	versionPredictions: Map<string, {version: string; bump_type: BumpType}>,
	reposWithChangesets: Set<string> = new Set(),
): ChangesetOperations => ({
	has_changesets: async (options) => ({
		ok: true,
		value: reposWithChangesets.has(options.repo.library.name),
	}),
	read_changesets: async () => ({ok: true, value: []}),
	predict_next_version: async (options) => {
		const prediction = versionPredictions.get(options.repo.library.name);
		if (!prediction) return null;
		return {ok: true, ...prediction};
	},
});

/**
 * Creates mock GitOperations for testing
 */
export const create_mock_git_ops = (overrides: Partial<GitOperations> = {}): GitOperations => ({
	current_branch_name: async () => ({ok: true, value: 'main'}),
	current_commit_hash: async () => ({ok: true, value: 'abc123'}),
	check_clean_workspace: async () => ({ok: true, value: true}),
	checkout: async () => ({ok: true}),
	pull: async () => ({ok: true}),
	switch_branch: async () => ({ok: true}),
	has_remote: async () => ({ok: true, value: false}),
	add: async () => ({ok: true}),
	commit: async () => ({ok: true}),
	add_and_commit: async () => ({ok: true}),
	has_changes: async () => ({ok: true, value: false}),
	get_changed_files: async () => ({ok: true, value: []}),
	tag: async () => ({ok: true}),
	push_tag: async () => ({ok: true}),
	stash: async () => ({ok: true}),
	stash_pop: async () => ({ok: true}),
	has_file_changed: async () => ({ok: true, value: false}),
	...overrides,
});

/**
 * Creates mock NpmOperations for testing
 */
export const create_mock_npm_ops = (overrides: Partial<NpmOperations> = {}): NpmOperations => ({
	wait_for_package: async () => ({ok: true}),
	check_package_available: async () => ({ok: true, value: true}),
	check_auth: async () => ({ok: true, username: 'testuser'}),
	check_registry: async () => ({ok: true}),
	install: async () => ({ok: true}),
	cache_clean: async () => ({ok: true}),
	...overrides,
});

/**
 * Creates mock BuildOperations for testing
 */
export const create_mock_build_ops = (
	overrides: Partial<BuildOperations> = {},
): BuildOperations => ({
	build_package: async () => ({ok: true}),
	...overrides,
});

/**
 * Creates a successful preflight mock with specified repos
 */
export const create_preflight_mock = (
	repos_with_changesets: Array<string> = [],
	repos_without_changesets: Array<string> = [],
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
		repos_without_changesets: new Set(repos_without_changesets),
	}),
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
				return {ok: false, message: `File not found: ${options.path}`};
			}
			return {ok: true, value: content};
		},
		writeFile: async (options) => {
			files.set(options.path, options.content);
			return {ok: true};
		},
		mkdir: async (options) => {
			dirs.add(options.path);
			return {ok: true};
		},
		exists: (options) => {
			return files.has(options.path) || dirs.has(options.path);
		},
		get: (path: string): string | undefined => files.get(path),
		set: (path: string, content: string): void => {
			files.set(path, content);
		},
	};
};

/**
 * Creates and populates fs ops from package.json files
 */
export const create_populated_fs_ops = (
	repos: Array<LocalRepo>,
	updated_versions?: Map<string, string>,
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
}

/**
 * Creates process operations that track which commands were spawned
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
			spawn: async (options) => {
				spawned_commands.push({
					cmd: options.cmd,
					args: options.args,
					cwd: options.spawn_options?.cwd?.toString() || '',
				});
				return {ok: true};
			},
		},
		get_spawned_commands: () => spawned_commands,
		get_commands_by_type: (cmd_name: string) =>
			spawned_commands.filter((c) => c.cmd === 'gro' && c.args[0] === cmd_name),
		get_package_names_from_cwd: (commands: Array<TrackedCommand>) =>
			commands.map((c) => c.cwd.split('/').pop() || ''),
	};
};

/**
 * Helper to create mock logger that tracks calls
 */
export const create_mock_logger = (): Logger & {
	debug_calls: Array<string>;
	info_calls: Array<string>;
	warn_calls: Array<string>;
} => {
	const debug_calls: Array<string> = [];
	const info_calls: Array<string> = [];
	const warn_calls: Array<string> = [];

	return {
		debug: vi.fn((msg: string) => debug_calls.push(msg)),
		info: vi.fn((msg: string) => info_calls.push(msg)),
		warn: vi.fn((msg: string) => warn_calls.push(msg)),
		error: vi.fn(),
		debug_calls,
		info_calls,
		warn_calls,
	} as unknown as Logger & {
		debug_calls: Array<string>;
		info_calls: Array<string>;
		warn_calls: Array<string>;
	};
};
