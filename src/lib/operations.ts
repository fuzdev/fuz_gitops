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
 * import {default_gitops_operations} from './operations_defaults.ts';
 * const ops = default_gitops_operations;
 * const result = await ops.git.current_commit_hash({cwd: '/path'});
 * if (!result.ok) {
 *   throw new TaskError(result.message);
 * }
 * const commit = result.value;
 * ```
 *
 * **Test usage:**
 * ```typescript
 * import {create_mock_gitops_ops} from './test_helpers.ts';
 * const ops = create_mock_gitops_ops({
 *   changeset: {has_changesets: async () => ({ok: true, value: false})}
 * });
 * const result = await publish_repos(repos, {...options, ops});
 * // Assert on result without any real git/npm calls
 * ```
 *
 * See `operations_defaults.ts` for real implementations, and the test-side mock
 * factories: `create_mock_gitops_ops` in `src/test/test_helpers.ts` (plain
 * objects with per-group overrides) and `create_fixture_gitops_ops` in
 * `src/test/fixtures/mock_operations.ts` (a fixture's changesets and
 * `package.json`s over `create_mock_gitops_ops`).
 *
 * @module
 */

import type { Result } from '@fuzdev/fuz_util/result.ts';
import type { FsError } from '@fuzdev/fuz_util/fs.ts';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import type { LocalRepo } from './local_repo.ts';
import type { ChangesetInfo } from './changeset_reader.ts';
import type { BumpType } from './version_utils.ts';
import type { PreflightResult, RunPreflightChecksOptions } from './preflight_checks.ts';
import type { WaitOptions } from './npm_registry.ts';

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
	}) => Promise<Result<{ value: boolean }, { message: string }>>;

	/**
	 * Reads all changeset files from a repo.
	 * Returns array of changeset info, or error if reading fails.
	 */
	read_changesets: (options: {
		repo: LocalRepo;
		log?: Logger;
	}) => Promise<Result<{ value: Array<ChangesetInfo> }, { message: string }>>;

	/**
	 * Predicts the next version based on changesets.
	 * Returns null if no changesets found (expected, not an error).
	 * Returns error `Result` if changesets exist but can't be read/parsed.
	 */
	predict_next_version: (options: {
		repo: LocalRepo;
		log?: Logger;
	}) => Promise<Result<{ version: string; bump_type: BumpType }, { message: string }> | null>;
}

/**
 * Git operations the publishing executor authors with: staging and committing
 * dependency updates and auto-changesets, and reading the commit it published.
 * All operations return `Result` instead of throwing errors. Where each repo
 * sits (branch, dirt, relation to origin) is `ReposOperations`'s to report.
 */
export interface GitOperations {
	/**
	 * Gets the current commit hash.
	 */
	current_commit_hash: (options?: {
		cwd?: string;
	}) => Promise<Result<{ value: string }, { message: string }>>;

	/**
	 * Stages files for commit.
	 */
	add: (options: {
		files: string | Array<string>;
		cwd?: string;
	}) => Promise<Result<object, { message: string }>>;

	/**
	 * Commits `files` alone (`git commit -- <files>`), leaving anything else
	 * staged out of the commit; `files` must be non-empty.
	 */
	commit: (options: {
		message: string;
		files: Array<string>;
		cwd?: string;
	}) => Promise<Result<object, { message: string }>>;
}

/**
 * Process operations for the commands the publishing executor runs in a repo
 * (`gro publish`, `gro deploy`).
 */
export interface ProcessOperations {
	/**
	 * Runs a command in the foreground and waits for it to exit: stdin is the
	 * terminal's, so a prompt (npm's 2FA one-time password) can be answered, and
	 * the child's output shows live — its stdout on ours, or on our stderr when
	 * `stdout` says so, and its stderr on ours. A failure carries the end of
	 * what the child wrote to stderr, bounded in lines and characters.
	 */
	run_interactive: (options: {
		cmd: string;
		args: Array<string>;
		cwd?: string;
		/**
		 * Where the child's stdout goes: our stdout, or our stderr when our stdout
		 * carries a machine-readable stream (JSON-lines events, a JSON or markdown
		 * report) the child's output would corrupt.
		 *
		 * @default 'stdout'
		 */
		stdout?: 'stdout' | 'stderr';
	}) => Promise<Result<object, { message: string; stderr_tail?: string }>>;
}

/**
 * What a `repos` command printed and how it exited, unparsed.
 */
export interface ReposCommandOutput {
	stdout: string;
	stderr: string;
	/** `0` for a report, `2` for an error document, `1` for a fatal I/O error (maybe no JSON). */
	exit_code: number;
}

/**
 * Operations running the Rust `repos` binary, which owns fleet git state.
 * Parsing its output is `repos_status_load.ts`'s, not the runner's.
 */
export interface ReposOperations {
	/**
	 * Runs `repos [--registry <path>] status [--fetch] <keys…> --json` in the
	 * process's cwd and returns what it printed, whatever its exit code. With
	 * `fetch`, `repos` fetches each entry from origin first, which writes
	 * remote-tracking refs and nothing else. Fails only when the binary didn't
	 * run to an exit: `not_found` when it isn't on `PATH`.
	 */
	status: (options: {
		keys: Array<string>;
		registry?: string;
		fetch?: boolean;
	}) => Promise<
		Result<{ output: ReposCommandOutput }, { kind: 'not_found' | 'failed'; message: string }>
	>;
}

/**
 * Build operations for validating packages compile before publishing.
 */
export interface BuildOperations {
	/**
	 * Builds a package using `gro build`.
	 */
	build_package: (options: {
		repo: LocalRepo;
	}) => Promise<Result<object, { message: string; output?: string }>>;
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
	}) => Promise<Result<object, { message: string }>>;

	/**
	 * Checks npm authentication status.
	 */
	check_auth: () => Promise<Result<{ username: string }, { message: string }>>;

	/**
	 * Checks if npm registry is reachable.
	 */
	check_registry: () => Promise<Result<object, { message: string }>>;
}

/**
 * Preflight validation operations run before publishing: building every
 * package the plan publishes, and npm authentication. Repo git state is the
 * readiness gate's, before preflight (see `repo_readiness.ts`).
 */
export interface PreflightOperations {
	/**
	 * Runs preflight validation checks before publishing.
	 */
	run_preflight_checks: (options: RunPreflightChecksOptions) => Promise<PreflightResult>;
}

/**
 * File system operations for reading and writing files.
 *
 * Errors are typed via `FsError` (`not_found | permission_denied |
 * already_exists | io_error`) so callers can branch on `kind` instead of
 * regex-matching `message`. See `@fuzdev/fuz_util/fs.ts`.
 */
export interface FsOperations {
	/**
	 * Reads a file from the file system.
	 */
	readFile: (options: {
		path: string;
		encoding: BufferEncoding;
	}) => Promise<Result<{ value: string }, FsError>>;

	/**
	 * Writes a file to the file system.
	 */
	writeFile: (options: { path: string; content: string }) => Promise<Result<object, FsError>>;

	/**
	 * Creates a directory, optionally with recursive creation.
	 */
	mkdir: (options: { path: string; recursive?: boolean }) => Promise<Result<object, FsError>>;

	/**
	 * Checks if a path exists on the file system.
	 */
	exists: (options: { path: string }) => Promise<boolean>;
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
	/** `repos status`, for the executor's re-check of each repo right before its publish. */
	repos: ReposOperations;
}
