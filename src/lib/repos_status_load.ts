/**
 * Reads fleet git state from the Rust `repos` binary: runs
 * `repos status <keys…> --json` through the injected `ReposOperations` and
 * parses what it printed with the `repos_status.ts` mirror.
 *
 * Parsing is pure (`parse_repos_status_output`): the format version is read
 * first, so a binary and npm package out of step fail with both versions and
 * the install command rather than a schema error; then the document is parsed
 * strictly as a report or, when it carries `error`, as an error document,
 * whose kind maps to a message saying what to change.
 *
 * @module
 */

import type { Result } from '@fuzdev/fuz_util/result.ts';
import { z } from 'zod';

import {
	REPOS_STATUS_FORMAT_VERSION,
	ReposStatusErrorReport,
	ReposStatusReport,
	type ReposStatusErrorBody
} from './repos_status.ts';
import type { ReposCommandOutput, ReposOperations } from './operations.ts';

/**
 * How to install the `repos` binary, run in a `fuz_gitops` checkout. The npm
 * package doesn't carry the binary, so the two are installed separately.
 */
export const REPOS_INSTALL_COMMAND = 'cargo install --path crates/fuz_repos --locked';

/**
 * The `repos` invocation a suggested fix names: `repos`, or `repos --registry
 * <path>` when the run passed one, the path POSIX-shell-quoted when it needs it.
 *
 * @param registry - the run's `--registry`, if any
 * @returns the command prefix, ready for a subcommand
 */
export const to_repos_command = (registry: string | undefined): string =>
	registry === undefined ? 'repos' : `repos --registry ${shell_quote(registry)}`;

const shell_quote = (arg: string): string =>
	/^[\w@%+=:,./-]+$/.test(arg) ? arg : `'${arg.replaceAll("'", `'\\''`)}'`;

/**
 * Why `repos status` gave no report: a message saying what to change, and the
 * error document when `repos` printed one.
 */
export interface ReposStatusLoadFailure {
	message: string;
	error?: ReposStatusErrorBody;
}

/**
 * Parses what `repos status --json` printed into its report, or a message
 * saying what went wrong and what to change.
 *
 * @param output - the command's stdout, stderr, and exit code
 * @returns the report, or a message naming the error document's kind and hint, a format version mismatch, or output that isn't a status document; an error document rides along as `error`
 */
export const parse_repos_status_output = (
	output: ReposCommandOutput
): Result<{ report: ReposStatusReport }, ReposStatusLoadFailure> => {
	const { stdout, stderr, exit_code } = output;

	let json: unknown;
	try {
		json = JSON.parse(stdout);
	} catch {
		// exit 1 is a fatal I/O error, which may print no JSON
		const detail = stderr.trim() || stdout.trim() || 'no output';
		return {
			ok: false,
			message: `\`repos status --json\` exited ${exit_code} without a JSON document: ${detail}`
		};
	}

	if (typeof json !== 'object' || json === null || Array.isArray(json)) {
		return { ok: false, message: '`repos status --json` printed JSON that is not a document' };
	}

	const version = 'version' in json ? json.version : undefined;
	if (version !== REPOS_STATUS_FORMAT_VERSION) {
		const fix =
			typeof version === 'number' && version > REPOS_STATUS_FORMAT_VERSION
				? "upgrade @fuzdev/fuz_gitops to match the binary, or install the binary from a fuz_gitops checkout at this package's version"
				: "install the binary from a fuz_gitops checkout at this package's version";
		return {
			ok: false,
			message:
				`the \`repos\` binary prints status format ${JSON.stringify(version ?? null)}, ` +
				`but this @fuzdev/fuz_gitops parses format ${REPOS_STATUS_FORMAT_VERSION}: ` +
				`${fix} (\`${REPOS_INSTALL_COMMAND}\`) — the npm package and the binary are installed separately`
		};
	}

	if ('error' in json) {
		const parsed = ReposStatusErrorReport.safeParse(json);
		if (!parsed.success) {
			return {
				ok: false,
				message: `\`repos status --json\` printed an error document this package can't parse:\n${z.prettifyError(parsed.error)}`
			};
		}
		return {
			ok: false,
			message: format_repos_status_error(parsed.data.error),
			error: parsed.data.error
		};
	}

	const parsed = ReposStatusReport.safeParse(json);
	if (!parsed.success) {
		return {
			ok: false,
			message: `\`repos status --json\` printed a report this package can't parse:\n${z.prettifyError(parsed.error)}`
		};
	}
	return { ok: true, report: parsed.data };
};

const format_repos_status_error = (error: ReposStatusErrorBody): string => {
	switch (error.kind) {
		case 'unknown_entry': {
			const suggestions = error.suggestions.length
				? ` — did you mean ${error.suggestions.map((s) => `\`${s}\``).join(', ')}?`
				: '';
			return `the gitops config lists \`${error.name}\`, which the registry (repos.toml) doesn't name${suggestions}`;
		}
		case 'registry_not_found':
			return (
				`${error.message}: the gitops tasks read repo state from a repos.toml registry — ` +
				`run inside its workspace, or pass \`--registry <path>\``
			);
		default: {
			const hint = error.hint ? `\n  hint: ${error.hint}` : '';
			return `\`repos status\` failed (${error.kind}): ${error.message}${hint}`;
		}
	}
};

/**
 * Runs `repos status <keys…> --json` and parses its report.
 *
 * @param options.keys - the registry keys to report on; must not be empty, since no targets means every entry
 * @param options.registry - a `repos.toml` to use instead of the one found walking up from the cwd
 * @param options.fetch - fetch each entry from origin first (`--fetch`), which writes remote-tracking refs and nothing else
 * @param options.repos_ops - the `repos` runner
 * @returns the report, or a message saying what went wrong and what to change
 */
export const load_repos_status = async (options: {
	keys: Array<string>;
	registry?: string;
	fetch?: boolean;
	repos_ops: ReposOperations;
}): Promise<Result<{ report: ReposStatusReport }, ReposStatusLoadFailure>> => {
	const { keys, registry, fetch, repos_ops } = options;
	if (keys.length === 0) {
		// `repos status` with no targets reports every entry
		return { ok: false, message: 'no registry keys to report on' };
	}
	const ran = await repos_ops.status(fetch ? { keys, registry, fetch } : { keys, registry });
	if (!ran.ok) {
		return {
			ok: false,
			message:
				ran.kind === 'not_found'
					? `the \`repos\` binary was not found on PATH: install it from a fuz_gitops checkout with \`${REPOS_INSTALL_COMMAND}\` — the npm package doesn't carry it`
					: `\`repos status\` didn't run: ${ran.message}`
		};
	}
	return parse_repos_status_output(ran.output);
};
