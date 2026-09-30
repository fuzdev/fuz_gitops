/**
 * Mock operations for testing.
 * Provides both basic mocks (always succeed) and configurable mocks for testing various scenarios.
 */

import type {
	GitOperations,
	NpmOperations,
	ProcessOperations,
	FsOperations,
	BuildOperations,
	PreflightOperations,
	GitopsOperations
} from '$lib/operations.ts';
import type { RepoFixtureSet } from './repo_fixture_types.ts';
import { create_mock_changeset_ops } from './mock_changeset_operations.ts';
import { create_mock_fs_ops, create_ready_repos_ops } from '../test_helpers.ts';

/* eslint-disable @typescript-eslint/require-await */

/**
 * Configuration for mock operations to simulate various conditions.
 */
export interface MockOperationsConfig {
	git?: {
		commit_hash?: string;
	};
	npm?: {
		authenticated?: boolean;
		registry_available?: boolean;
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
	// Commit info - return stable hash
	current_commit_hash: async () => ({ ok: true, value: 'fixture-commit-hash-001' }),

	// Staging/commit - no-op for read-only fixtures
	add: async () => ({ ok: true }),
	commit: async () => ({ ok: true })
});

/**
 * Create basic npm operations that always succeed.
 */
export const create_mock_npm_ops = (): NpmOperations => ({
	// Auth check - always authenticated for tests
	check_auth: async () => ({ ok: true, username: 'test-user' }),

	// Registry check - always available for tests
	check_registry: async () => ({ ok: true }),

	// Registry check - simulate package available immediately
	wait_for_package: async () => ({ ok: true })
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
				return { ok: true, stdout: 'Build successful' };
			}
			if (options.args[0] === 'publish') {
				return { ok: true, stdout: 'Published' };
			}
		}
		return { ok: true };
	}
});

/**
 * Create fixture-populated file system operations.
 *
 * Wraps the shared `create_mock_fs_ops` from `../test_helpers.ts` and pre-populates
 * each fixture repo's `package.json` content. Any path not explicitly set will
 * behave per the shared mock's convention (Result<ok:false> on unknown reads),
 * which surfaces fixture-setup mistakes as loud failures instead of silently
 * returning `{}`.
 */
export const create_fixture_fs_ops = (fixture: RepoFixtureSet): FsOperations => {
	const fs = create_mock_fs_ops();
	for (const repo of fixture.repos) {
		const path = `/fixtures/repos/${fixture.name}/${repo.repo_name}/package.json`;
		fs.set(path, JSON.stringify(repo.package_json, null, '\t'));
	}
	return fs;
};

/**
 * Create basic build operations.
 * Fixtures don't need real builds.
 */
export const create_mock_build_ops = (): BuildOperations => ({
	build_package: async () => ({ ok: true })
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
			repos_without_changesets
		})
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
	fs: create_fixture_fs_ops(fixture),
	build: create_mock_build_ops(),
	preflight: create_mock_preflight_ops(fixture),
	repos: create_ready_repos_ops()
});

/**
 * Create configurable git operations for testing specific scenarios.
 */
export const create_configurable_git_ops = (
	config: MockOperationsConfig['git'] = {}
): GitOperations => ({
	// Commit info
	current_commit_hash: async () => ({
		ok: true,
		value: config.commit_hash || 'fixture-commit-hash-001'
	}),

	// Staging/commit
	add: async () => ({ ok: true }),
	commit: async () => ({ ok: true })
});

/**
 * Create configurable npm operations for testing specific scenarios.
 */
export const create_configurable_npm_ops = (
	config: MockOperationsConfig['npm'] = {}
): NpmOperations => ({
	// Auth check
	check_auth: async () => {
		if (config.authenticated === false) {
			return { ok: false, message: 'Not authenticated to npm' };
		}
		return { ok: true, username: 'test-user' };
	},

	// Registry check
	check_registry: async () => {
		if (config.registry_available === false) {
			return { ok: false, message: 'NPM registry unavailable' };
		}
		return { ok: true };
	},

	// Wait for package
	wait_for_package: async () => {
		if (config.wait_timeout) {
			return { ok: false, message: 'Timeout waiting for package', timeout: true };
		}
		return { ok: true };
	}
});

/**
 * Create configurable build operations for testing specific scenarios.
 */
export const create_configurable_build_ops = (
	config: MockOperationsConfig['build'] = {}
): BuildOperations => ({
	build_package: async () => {
		if (config.build_fails) {
			return {
				ok: false,
				message: config.build_error_message || 'Build failed',
				stderr: 'TypeScript compilation errors'
			};
		}
		return { ok: true };
	}
});

/**
 * Create configurable preflight operations for testing specific scenarios.
 */
export const create_configurable_preflight_ops = (
	fixture: RepoFixtureSet,
	config: MockOperationsConfig['preflight'] = {}
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
					repos_without_changesets
				};
			}
			return {
				ok: true,
				warnings: config.warnings || [],
				errors: config.errors || [],
				repos_with_changesets,
				repos_without_changesets
			};
		}
	};
};

/**
 * Create complete configurable gitops operations.
 */
export const create_configurable_gitops_ops = (
	fixture: RepoFixtureSet,
	config: MockOperationsConfig = {}
): GitopsOperations => ({
	changeset: create_mock_changeset_ops(fixture),
	git: create_configurable_git_ops(config.git),
	npm: create_configurable_npm_ops(config.npm),
	process: create_mock_process_ops(),
	fs: create_fixture_fs_ops(fixture),
	build: create_configurable_build_ops(config.build),
	preflight: create_configurable_preflight_ops(fixture, config.preflight),
	repos: create_ready_repos_ops()
});

//
// Specific failure scenario factories
//

/**
 * Create npm operations that simulate authentication failure.
 */
export const create_unauthenticated_npm_ops = (): NpmOperations => ({
	...create_mock_npm_ops(),
	check_auth: async () => ({ ok: false, message: 'Not authenticated to npm registry' })
});

/**
 * Create npm operations that simulate registry being down.
 */
export const create_unavailable_registry_npm_ops = (): NpmOperations => ({
	...create_mock_npm_ops(),
	check_registry: async () => ({ ok: false, message: 'NPM registry is unavailable' })
});

/**
 * Create build operations that simulate build failure.
 */
export const create_failing_build_ops = (): BuildOperations => ({
	build_package: async () => ({
		ok: false,
		message: 'Build failed with TypeScript errors',
		stderr: 'error TS2322: Type string is not assignable to type number'
	})
});
