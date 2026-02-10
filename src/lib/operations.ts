/**
 * Operations interfaces for dependency injection.
 *
 * This is the core pattern enabling testability without mocks.
 * All side effects (git, npm, fs, process) are abstracted into interfaces.
 *
 * **Design principles:**
 * - All operations accept a single `options` object parameter
 * - All fallible operations return `Result` from `@fuzdev/fuz_util`
 * - Never throw `Error` in operations - return `Result` with `ok: false`
 * - Use `null` for expected "not found" cases (not errors)
 * - Include `log?: Logger` in options where logging is useful
 *
 * **Production usage:**
 * ```typescript
 * import {default_gitops_operations} from './operations_defaults.js';
 * const result = await ops.git.current_branch_name({cwd: '/path'});
 * if (!result.ok) {
 *   throw new TaskError(result.message);
 * }
 * const branch = result.value;
 * ```
 *
 * **Test usage:**
 * ```typescript
 * const mock_ops = create_mock_operations();
 * const result = await publish_repos(repos, {...options, ops: mock_ops});
 * // Assert on result without any real git/npm calls
 * ```
 *
 * See `operations_defaults.ts` for real implementations.
 * See test files (*.test.ts) for mock implementations.
 *
 * @module
 */

import type {Result} from '@fuzdev/fuz_util/result.js';
import type {Logger} from '@fuzdev/fuz_util/log.js';
import type {FsOperations} from '@fuzdev/fuz_util/fs_operations.js';
import type {SpawnOptions} from 'node:child_process';

import type {LocalRepo} from './local_repo.js';
import type {ChangesetInfo} from './changeset_reader.js';
import type {BumpType} from './semver.js';
import type {PreflightOptions, PreflightResult} from './preflight_checks.js';
import type {WaitOptions} from './npm_registry.js';

/**
 * Changeset operations for reading and predicting versions from `.changeset/*.md` files.
 */
export interface ChangesetOperations {
	/**
	 * Checks if a repo has any changeset files.
	 * Returns true if changesets exist, false if none found.
	 */
	has_changesets: (options: {
		repo: LocalRepo;
	}) => Promise<Result<{value: boolean}, {message: string}>>;

	/**
	 * Reads all changeset files from a repo.
	 * Returns array of changeset info, or error if reading fails.
	 */
	read_changesets: (options: {
		repo: LocalRepo;
		log?: Logger;
	}) => Promise<Result<{value: Array<ChangesetInfo>}, {message: string}>>;

	/**
	 * Predicts the next version based on changesets.
	 * Returns null if no changesets found (expected, not an error).
	 * Returns error Result if changesets exist but can't be read/parsed.
	 */
	predict_next_version: (options: {
		repo: LocalRepo;
		log?: Logger;
	}) => Promise<Result<{version: string; bump_type: BumpType}, {message: string}> | null>;
}

/**
 * Git operations for branch management, commits, tags, and workspace state.
 * All operations return `Result` instead of throwing errors.
 */
export interface GitOperations {
	/**
	 * Gets the current branch name.
	 */
	current_branch_name: (options?: {
		cwd?: string;
	}) => Promise<Result<{value: string}, {message: string}>>;

	/**
	 * Gets the current commit hash.
	 */
	current_commit_hash: (options?: {
		branch?: string;
		cwd?: string;
	}) => Promise<Result<{value: string}, {message: string}>>;

	/**
	 * Checks if the workspace is clean (no uncommitted changes).
	 */
	check_clean_workspace: (options?: {
		cwd?: string;
	}) => Promise<Result<{value: boolean}, {message: string}>>;

	/**
	 * Checks out a branch.
	 */
	checkout: (options: {branch: string; cwd?: string}) => Promise<Result<object, {message: string}>>;

	/**
	 * Pulls changes from remote.
	 */
	pull: (options?: {
		origin?: string;
		branch?: string;
		cwd?: string;
	}) => Promise<Result<object, {message: string}>>;

	/**
	 * Switches to a branch, optionally pulling.
	 */
	switch_branch: (options: {
		branch: string;
		pull?: boolean;
		cwd?: string;
	}) => Promise<Result<object, {message: string}>>;

	/**
	 * Checks if a remote exists.
	 */
	has_remote: (options?: {
		remote?: string;
		cwd?: string;
	}) => Promise<Result<{value: boolean}, {message: string}>>;

	/**
	 * Stages files for commit.
	 */
	add: (options: {
		files: string | Array<string>;
		cwd?: string;
	}) => Promise<Result<object, {message: string}>>;

	/**
	 * Creates a commit.
	 */
	commit: (options: {message: string; cwd?: string}) => Promise<Result<object, {message: string}>>;

	/**
	 * Stages files and creates a commit.
	 */
	add_and_commit: (options: {
		files: string | Array<string>;
		message: string;
		cwd?: string;
	}) => Promise<Result<object, {message: string}>>;

	/**
	 * Checks if there are any uncommitted changes.
	 */
	has_changes: (options?: {cwd?: string}) => Promise<Result<{value: boolean}, {message: string}>>;

	/**
	 * Gets a list of changed files.
	 */
	get_changed_files: (options?: {
		cwd?: string;
	}) => Promise<Result<{value: Array<string>}, {message: string}>>;

	/**
	 * Creates a git tag.
	 */
	tag: (options: {
		tag_name: string;
		message?: string;
		cwd?: string;
	}) => Promise<Result<object, {message: string}>>;

	/**
	 * Pushes a tag to remote.
	 */
	push_tag: (options: {
		tag_name: string;
		origin?: string;
		cwd?: string;
	}) => Promise<Result<object, {message: string}>>;

	/**
	 * Stashes uncommitted changes.
	 */
	stash: (options?: {message?: string; cwd?: string}) => Promise<Result<object, {message: string}>>;

	/**
	 * Pops the most recent stash.
	 */
	stash_pop: (options?: {cwd?: string}) => Promise<Result<object, {message: string}>>;

	/**
	 * Checks if a specific file changed between two commits.
	 */
	has_file_changed: (options: {
		from_commit: string;
		to_commit: string;
		file_path: string;
		cwd?: string;
	}) => Promise<Result<{value: boolean}, {message: string}>>;
}

/**
 * Process spawning operations for running shell commands.
 */
export interface ProcessOperations {
	/**
	 * Spawns a child process and waits for completion.
	 */
	spawn: (options: {
		cmd: string;
		args: Array<string>;
		spawn_options?: SpawnOptions;
	}) => Promise<Result<{stdout?: string; stderr?: string}, {message: string; stderr?: string}>>;
}

/**
 * Build operations for validating packages compile before publishing.
 */
export interface BuildOperations {
	/**
	 * Builds a package using gro build.
	 */
	build_package: (options: {
		repo: LocalRepo;
		log?: Logger;
	}) => Promise<Result<object, {message: string; output?: string}>>;
}

/**
 * NPM registry operations for package availability checks and authentication.
 * Includes exponential backoff for waiting on package propagation.
 */
export interface NpmOperations {
	/**
	 * Waits for a package version to be available on NPM.
	 * Uses exponential backoff with configurable timeout.
	 */
	wait_for_package: (options: {
		pkg: string;
		version: string;
		wait_options?: WaitOptions;
		log?: Logger;
	}) => Promise<Result<object, {message: string; timeout?: boolean}>>;

	/**
	 * Checks if a package version is available on NPM.
	 */
	check_package_available: (options: {
		pkg: string;
		version: string;
		log?: Logger;
	}) => Promise<Result<{value: boolean}, {message: string}>>;

	/**
	 * Checks npm authentication status.
	 */
	check_auth: () => Promise<Result<{username: string}, {message: string}>>;

	/**
	 * Checks if npm registry is reachable.
	 */
	check_registry: () => Promise<Result<object, {message: string}>>;

	/**
	 * Installs npm dependencies.
	 */
	install: (options?: {
		cwd?: string;
	}) => Promise<Result<object, {message: string; stderr?: string}>>;

	/**
	 * Cleans the npm cache.
	 * Uses `npm cache clean --force` to clear stale cache entries.
	 */
	cache_clean: () => Promise<Result<object, {message: string}>>;
}

/**
 * Preflight validation operations to ensure repos are ready for publishing.
 * Validates workspace state, branches, builds, and npm authentication.
 */
export interface PreflightOperations {
	/**
	 * Runs preflight validation checks before publishing.
	 */
	run_preflight_checks: (options: {
		repos: Array<LocalRepo>;
		preflight_options: PreflightOptions;
		git_ops?: GitOperations;
		npm_ops?: NpmOperations;
		build_ops?: BuildOperations;
		changeset_ops?: ChangesetOperations;
	}) => Promise<PreflightResult>;
}

/**
 * Combined operations interface grouping all gitops functionality.
 * This is the main interface injected into publishing and validation workflows.
 */
export interface GitopsOperations {
	changeset: ChangesetOperations;
	git: GitOperations;
	process: ProcessOperations;
	npm: NpmOperations;
	preflight: PreflightOperations;
	fs: FsOperations;
	build: BuildOperations;
}
