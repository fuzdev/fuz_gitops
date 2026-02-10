/**
 * Mock operations for testing.
 * Provides both basic mocks (always succeed) and configurable mocks for testing various scenarios.
 */

import type {FsOperations} from '@fuzdev/fuz_util/fs_operations.js';

import type {
	GitOperations,
	NpmOperations,
	ProcessOperations,
	BuildOperations,
	PreflightOperations,
	GitopsOperations,
} from '$lib/operations.js';

import type {RepoFixtureSet} from './repo_fixture_types.js';
import {create_mock_changeset_ops} from './mock_changeset_operations.js';

/* eslint-disable @typescript-eslint/require-await */

/**
 * Configuration for mock operations to simulate various conditions.
 */
export interface MockOperationsConfig {
	git?: {
		workspace_clean?: boolean;
		current_branch?: string;
		has_remote?: boolean;
		has_changes?: boolean;
		commit_hash?: string;
		checkout_fails?: boolean;
		pull_fails?: boolean;
	};
	npm?: {
		authenticated?: boolean;
		registry_available?: boolean;
		install_fails?: boolean;
		package_available?: boolean;
		wait_timeout?: boolean;
	};
	build?: {
		build_fails?: boolean;
		build_error_message?: string;
	};
	preflight?: {
		fails?: boolean;
		errors?: Array<string>;
		warnings?: Array<string>;
	};
}

/**
 * Create basic git operations that always succeed.
 * Used for testing normal flow without errors.
 */
export const create_mock_git_ops = (): GitOperations => ({
	// Branch info - fixtures always on main
	current_branch_name: async () => ({ok: true, value: 'main'}),

	// Commit info - return stable hash
	current_commit_hash: async () => ({ok: true, value: 'fixture-commit-hash-001'}),

	// Workspace state - fixtures always clean
	check_clean_workspace: async () => ({ok: true, value: true}),

	// Branch operations - no-op for fixtures
	checkout: async () => ({ok: true}),
	pull: async () => ({ok: true}), // No-op - fixtures have no remotes
	switch_branch: async () => ({ok: true}),

	// Remote operations - fixtures have no remotes
	has_remote: async () => ({ok: true, value: false}),

	// Staging/commit - no-op for read-only fixtures
	add: async () => ({ok: true}),
	commit: async () => ({ok: true}),
	add_and_commit: async () => ({ok: true}),

	// Change detection - fixtures never change
	has_changes: async () => ({ok: true, value: false}),
	get_changed_files: async () => ({ok: true, value: []}),
	has_file_changed: async () => ({ok: true, value: false}),

	// Tags - no-op for fixtures
	tag: async () => ({ok: true}),
	push_tag: async () => ({ok: true}),

	// Stash - no-op for clean fixtures
	stash: async () => ({ok: true}),
	stash_pop: async () => ({ok: true}),
});

/**
 * Create basic npm operations that always succeed.
 */
export const create_mock_npm_ops = (): NpmOperations => ({
	// Install - no-op, fixtures don't need deps installed
	install: async () => ({ok: true}),

	// Auth check - always authenticated for tests
	check_auth: async () => ({ok: true, username: 'test-user'}),

	// Registry check - always available for tests
	check_registry: async () => ({ok: true}),

	// Package availability check
	check_package_available: async () => ({ok: true, value: true}),

	// Registry check - simulate package available immediately
	wait_for_package: async () => ({ok: true}),

	// Cache clean - no-op for tests
	cache_clean: async () => ({ok: true}),
});

/**
 * Create basic process operations.
 * Avoids spawning real processes.
 */
export const create_mock_process_ops = (): ProcessOperations => ({
	spawn: async (options) => {
		// Simulate success for common commands
		if (options.cmd === 'gro') {
			if (options.args[0] === 'build') {
				return {ok: true, stdout: 'Build successful'};
			}
			if (options.args[0] === 'publish') {
				return {ok: true, stdout: 'Published'};
			}
		}
		return {ok: true};
	},
});

/**
 * Create basic file system operations.
 * Returns predictable values without actual I/O.
 */
export const create_mock_fs_ops = (fixture: RepoFixtureSet): FsOperations => {
	// Pre-compute package.json contents for each repo
	const package_jsons: Map<string, string> = new Map();
	for (const repo of fixture.repos) {
		const path = `/fixtures/repos/${fixture.name}/${repo.repo_name}/package.json`;
		package_jsons.set(path, JSON.stringify(repo.package_json, null, '\t'));
	}

	const not_implemented = {ok: false as const, message: 'not implemented in mock'};

	return {
		read_file: async (options) => {
			const content = package_jsons.get(options.path);
			if (content) {
				return {ok: true, value: content};
			}
			// Default for unknown files
			return {ok: true, value: '{}'};
		},
		read_file_buffer: async () => not_implemented,
		write_file: async () => ({ok: true}),
		stat: async () => not_implemented,
		exists: async () => false,
		readdir: async () => not_implemented,
		mkdir: async () => not_implemented,
		rm: async () => not_implemented,
	};
};

/**
 * Create basic build operations.
 * Fixtures don't need real builds.
 */
export const create_mock_build_ops = (): BuildOperations => ({
	build_package: async () => ({ok: true}),
});

/**
 * Create basic preflight operations.
 * Returns success for all fixture repos.
 */
export const create_mock_preflight_ops = (fixture: RepoFixtureSet): PreflightOperations => {
	// Determine which repos have changesets
	const repos_with_changesets: Set<string> = new Set();
	const repos_without_changesets: Set<string> = new Set();

	for (const repo of fixture.repos) {
		if (repo.changesets && repo.changesets.length > 0) {
			repos_with_changesets.add(repo.package_json.name);
		} else {
			repos_without_changesets.add(repo.package_json.name);
		}
	}

	return {
		run_preflight_checks: async () => ({
			ok: true,
			warnings: [],
			errors: [],
			repos_with_changesets,
			repos_without_changesets,
		}),
	};
};

/**
 * Create complete gitops operations for a fixture using basic mocks.
 */
export const create_mock_gitops_ops = (fixture: RepoFixtureSet): GitopsOperations => ({
	changeset: create_mock_changeset_ops(fixture),
	git: create_mock_git_ops(),
	npm: create_mock_npm_ops(),
	process: create_mock_process_ops(),
	fs: create_mock_fs_ops(fixture),
	build: create_mock_build_ops(),
	preflight: create_mock_preflight_ops(fixture),
});

/**
 * Create configurable git operations for testing specific scenarios.
 */
export const create_configurable_git_ops = (
	config: MockOperationsConfig['git'] = {},
): GitOperations => ({
	// Branch info
	current_branch_name: async () => ({
		ok: true,
		value: config.current_branch || 'main',
	}),

	// Commit info
	current_commit_hash: async () => ({
		ok: true,
		value: config.commit_hash || 'fixture-commit-hash-001',
	}),

	// Workspace state
	check_clean_workspace: async () => ({
		ok: true,
		value: config.workspace_clean !== false, // Default to true
	}),

	// Branch operations
	checkout: async () => {
		if (config.checkout_fails) {
			return {ok: false, message: 'Failed to checkout branch'};
		}
		return {ok: true};
	},

	pull: async () => {
		if (config.pull_fails) {
			return {ok: false, message: 'Failed to pull from remote'};
		}
		return {ok: true};
	},

	switch_branch: async () => ({ok: true}),

	// Remote operations
	has_remote: async () => ({
		ok: true,
		value: config.has_remote !== false, // Default to false for fixtures
	}),

	// Staging/commit
	add: async () => ({ok: true}),
	commit: async () => ({ok: true}),
	add_and_commit: async () => ({ok: true}),

	// Change detection
	has_changes: async () => ({
		ok: true,
		value: config.has_changes || false,
	}),
	get_changed_files: async () => ({
		ok: true,
		value: config.has_changes ? ['package.json'] : [],
	}),
	has_file_changed: async () => ({
		ok: true,
		value: config.has_changes || false,
	}),

	// Tags
	tag: async () => ({ok: true}),
	push_tag: async () => ({ok: true}),

	// Stash
	stash: async () => ({ok: true}),
	stash_pop: async () => ({ok: true}),
});

/**
 * Create configurable npm operations for testing specific scenarios.
 */
export const create_configurable_npm_ops = (
	config: MockOperationsConfig['npm'] = {},
): NpmOperations => ({
	// Install
	install: async () => {
		if (config.install_fails) {
			return {ok: false, message: 'npm install failed', stderr: 'npm ERR! install failed'};
		}
		return {ok: true};
	},

	// Auth check
	check_auth: async () => {
		if (config.authenticated === false) {
			return {ok: false, message: 'Not authenticated to npm'};
		}
		return {ok: true, username: 'test-user'};
	},

	// Registry check
	check_registry: async () => {
		if (config.registry_available === false) {
			return {ok: false, message: 'NPM registry unavailable'};
		}
		return {ok: true};
	},

	// Package availability
	check_package_available: async () => ({
		ok: true,
		value: config.package_available !== false,
	}),

	// Wait for package
	wait_for_package: async () => {
		if (config.wait_timeout) {
			return {ok: false, message: 'Timeout waiting for package', timeout: true};
		}
		return {ok: true};
	},

	// Cache clean
	cache_clean: async () => ({ok: true}),
});

/**
 * Create configurable build operations for testing specific scenarios.
 */
export const create_configurable_build_ops = (
	config: MockOperationsConfig['build'] = {},
): BuildOperations => ({
	build_package: async () => {
		if (config.build_fails) {
			return {
				ok: false,
				message: config.build_error_message || 'Build failed',
				stderr: 'TypeScript compilation errors',
			};
		}
		return {ok: true};
	},
});

/**
 * Create configurable preflight operations for testing specific scenarios.
 */
export const create_configurable_preflight_ops = (
	fixture: RepoFixtureSet,
	config: MockOperationsConfig['preflight'] = {},
): PreflightOperations => {
	// Determine which repos have changesets
	const repos_with_changesets: Set<string> = new Set();
	const repos_without_changesets: Set<string> = new Set();

	for (const repo of fixture.repos) {
		if (repo.changesets && repo.changesets.length > 0) {
			repos_with_changesets.add(repo.package_json.name);
		} else {
			repos_without_changesets.add(repo.package_json.name);
		}
	}

	return {
		run_preflight_checks: async () => {
			if (config.fails) {
				return {
					ok: false,
					warnings: config.warnings || [],
					errors: config.errors || ['Preflight checks failed'],
					repos_with_changesets,
					repos_without_changesets,
				};
			}
			return {
				ok: true,
				warnings: config.warnings || [],
				errors: config.errors || [],
				repos_with_changesets,
				repos_without_changesets,
			};
		},
	};
};

/**
 * Create complete configurable gitops operations.
 */
export const create_configurable_gitops_ops = (
	fixture: RepoFixtureSet,
	config: MockOperationsConfig = {},
): GitopsOperations => ({
	changeset: create_mock_changeset_ops(fixture),
	git: create_configurable_git_ops(config.git),
	npm: create_configurable_npm_ops(config.npm),
	process: create_mock_process_ops(),
	fs: create_mock_fs_ops(fixture),
	build: create_configurable_build_ops(config.build),
	preflight: create_configurable_preflight_ops(fixture, config.preflight),
});

//
// Specific failure scenario factories
//

/**
 * Create git operations that simulate a dirty workspace.
 */
export const create_dirty_workspace_git_ops = (): GitOperations => ({
	...create_mock_git_ops(),
	check_clean_workspace: async () => ({ok: true, value: false}),
	has_changes: async () => ({ok: true, value: true}),
	get_changed_files: async () => ({ok: true, value: ['package.json', 'src/index.ts']}),
});

/**
 * Create git operations that simulate being on wrong branch.
 */
export const create_wrong_branch_git_ops = (): GitOperations => ({
	...create_mock_git_ops(),
	current_branch_name: async () => ({ok: true, value: 'feature-branch'}),
});

/**
 * Create npm operations that simulate authentication failure.
 */
export const create_unauthenticated_npm_ops = (): NpmOperations => ({
	...create_mock_npm_ops(),
	check_auth: async () => ({ok: false, message: 'Not authenticated to npm registry'}),
});

/**
 * Create npm operations that simulate registry being down.
 */
export const create_unavailable_registry_npm_ops = (): NpmOperations => ({
	...create_mock_npm_ops(),
	check_registry: async () => ({ok: false, message: 'NPM registry is unavailable'}),
});

/**
 * Create build operations that simulate build failure.
 */
export const create_failing_build_ops = (): BuildOperations => ({
	build_package: async () => ({
		ok: false,
		message: 'Build failed with TypeScript errors',
		stderr: 'error TS2322: Type string is not assignable to type number',
	}),
});
