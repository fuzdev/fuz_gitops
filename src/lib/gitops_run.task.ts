import { TaskError, type Task } from '@fuzdev/gro';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { z } from 'zod';
import { map_concurrent_settled } from '@fuzdev/fuz_util/async.ts';
import { spawn_out } from '@fuzdev/fuz_util/process.ts';
import { to_error_message } from '@fuzdev/fuz_util/error.ts';
import { writeFile } from 'node:fs/promises';
import { styleText as st } from 'node:util';

import { resolve_gitops_repos } from './gitops_task_helpers.ts';
import { GITOPS_CONCURRENCY_DEFAULT, GITOPS_CONFIG_PATH_DEFAULT } from './gitops_constants.ts';
import type { ReposOperations } from './operations.ts';
import { default_repos_operations } from './operations_defaults.ts';
import { output_is_machine, route_human_output } from './output_helpers.ts';

/** @nodocs */
export const Args = z.strictObject({
	// Positional rest args (gro convention) so `gro gitops_run "npm test"` works;
	// joined with spaces and passed to `sh -c`, so quote commands that contain flags.
	_: z.array(z.string()).meta({ description: 'shell command to run in each repo' }).default([]),
	config: z
		.string()
		.meta({ description: 'path to the gitops config file' })
		.default(GITOPS_CONFIG_PATH_DEFAULT),
	registry: z
		.string()
		.meta({
			description:
				'path to the repos.toml registry, when `repos` would not find it walking up from the cwd'
		})
		.optional(),
	concurrency: z
		.number()
		.int()
		.min(1)
		.meta({ description: 'maximum number of repos to run in parallel' })
		.default(GITOPS_CONCURRENCY_DEFAULT),
	format: z.enum(['text', 'json']).meta({ description: 'output format' }).default('text'),
	outfile: z
		.string()
		.meta({ description: 'with --format json, write clean JSON to this file instead of stdout' })
		.optional()
});
export type Args = z.infer<typeof Args>;

/**
 * How a command run in one repo ended: it exited with a code, a signal killed
 * it, or it never started.
 *
 * @nodocs
 */
export type GitopsRunOutcome =
	| { kind: 'exited'; code: number }
	| { kind: 'signaled'; signal: string }
	| { kind: 'error'; message: string };

/**
 * What a command run in one repo printed, and how it ended.
 *
 * @nodocs
 */
export interface GitopsRunCommandOutput {
	outcome: GitopsRunOutcome;
	stdout: string;
	stderr: string;
}

/**
 * One repo's result, as the JSON output carries it.
 *
 * @nodocs
 */
export interface GitopsRunResult {
	repo_name: string;
	repo_dir: string;
	status: 'success' | 'failure';
	/** The command's exit code; `null` when a signal killed it or it never started. */
	exit_code: number | null;
	/** The signal that killed the command, else `null`. */
	signal: string | null;
	stdout: string;
	stderr: string;
	duration_ms: number;
	/** Why the command never started. */
	error?: string;
}

/**
 * The side effects `run_gitops_run` reaches through, injectable for tests.
 *
 * @nodocs
 */
export interface GitopsRunDeps {
	/** Runs `repos status` to resolve the config's repos. */
	repos_ops: ReposOperations;
	/** Runs `command` through `sh -c` in `cwd`, capturing its output. */
	run_command: (options: { command: string; cwd: string }) => Promise<GitopsRunCommandOutput>;
}

const default_gitops_run_deps: GitopsRunDeps = {
	repos_ops: default_repos_operations,
	run_command: async ({ command, cwd }) => {
		// through a shell, so pipes, redirects, and globs work
		const { result, stdout, stderr } = await spawn_out('sh', ['-c', command], { cwd });
		const outcome: GitopsRunOutcome =
			result.kind === 'exited'
				? { kind: 'exited', code: result.code }
				: result.kind === 'signaled'
					? { kind: 'signaled', signal: result.signal }
					: { kind: 'error', message: to_error_message(result.error) };
		return { outcome, stdout: stdout ?? '', stderr: stderr ?? '' };
	}
};

/** @nodocs */
export const task: Task<Args> = {
	Args,
	summary: 'run a shell command across all repos in parallel',
	run: async ({ args, log }) => {
		await run_gitops_run(args, log);
	}
};

/**
 * Runs `gro gitops_run`: resolves the config's repos through `repos status`,
 * refusing a missing one, runs the command in each, `concurrency` at a time,
 * and reports every result. Under `--format json` without `--outfile`, the log
 * goes to stderr and stdout carries the JSON alone (`route_human_output`).
 *
 * @throws {TaskError} when no command is given, resolving the repos fails, or the command fails in any repo (after reporting)
 * @nodocs
 */
export const run_gitops_run = async (
	args: Args,
	log: Logger,
	deps: Partial<GitopsRunDeps> = {}
): Promise<void> => {
	const { repos_ops, run_command } = { ...default_gitops_run_deps, ...deps };
	const { _, config, registry, concurrency, format, outfile } = args;
	const write_stdout = route_human_output(log, output_is_machine(format, outfile));

	const command = _.join(' ').trim();
	if (!command) {
		throw new TaskError('No command provided, e.g. `gro gitops_run "npm test"`');
	}

	// Resolve repo paths through `repos status` (no library-metadata loading needed);
	// a configured repo that's missing or not a repo fails the run, naming it
	const { local_repo_paths: repos } = await resolve_gitops_repos({
		config,
		registry,
		log,
		repos_ops
	});

	log.info(
		`Running ${st('cyan', command)} across ${repos.length} repos (concurrency: ${concurrency})`
	);

	const start_time = performance.now();

	const settled = await map_concurrent_settled(
		repos,
		concurrency,
		async ({ repo_name, repo_dir }): Promise<GitopsRunResult> => {
			const repo_start = performance.now();
			const { outcome, stdout, stderr } = await run_command({ command, cwd: repo_dir });
			return {
				repo_name,
				repo_dir,
				status: outcome.kind === 'exited' && outcome.code === 0 ? 'success' : 'failure',
				exit_code: outcome.kind === 'exited' ? outcome.code : null,
				signal: outcome.kind === 'signaled' ? outcome.signal : null,
				stdout,
				stderr,
				duration_ms: performance.now() - repo_start,
				...(outcome.kind === 'error' && { error: outcome.message })
			};
		}
	);

	const total_duration_ms = performance.now() - start_time;

	// Process results; a rejection is `run_command` throwing, which the default never does
	const successes: Array<GitopsRunResult> = [];
	const failures: Array<GitopsRunResult> = [];
	settled.forEach((result, i) => {
		if (result.status === 'fulfilled') {
			(result.value.status === 'success' ? successes : failures).push(result.value);
		} else {
			const { repo_name, repo_dir } = repos[i]!;
			failures.push({
				repo_name,
				repo_dir,
				status: 'failure',
				exit_code: null,
				signal: null,
				stdout: '',
				stderr: '',
				duration_ms: 0,
				error: to_error_message(result.reason)
			});
		}
	});

	// Output results based on format
	if (format === 'json') {
		const json_output = {
			command,
			concurrency,
			repos: [...successes, ...failures],
			summary: {
				total: repos.length,
				success: successes.length,
				failure: failures.length,
				duration_ms: Math.round(total_duration_ms)
			}
		};
		const json = JSON.stringify(json_output, null, 2);
		if (outfile) {
			// Clean machine-readable output, free of gro's logging prefixes.
			await writeFile(outfile, json);
			log.info(`wrote JSON output to ${outfile}`);
		} else {
			write_stdout(json);
		}
	} else {
		log_text_results({ successes, failures, total: repos.length, total_duration_ms }, log);
	}

	// Exit with error if any failures (so CI fails)
	if (failures.length > 0) {
		throw new TaskError(`${failures.length} repos failed`);
	}
};

const log_text_results = (
	data: {
		successes: Array<GitopsRunResult>;
		failures: Array<GitopsRunResult>;
		total: number;
		total_duration_ms: number;
	},
	log: Logger
): void => {
	const { successes, failures, total, total_duration_ms } = data;
	log.info(''); // blank line

	// Show successes, including the command's output so read-only commands
	// (`git rev-parse`, `cat package.json`, …) are useful without `--format json`.
	if (successes.length > 0) {
		log.info(st('green', `✓ ${successes.length} succeeded:`));
		for (const result of successes) {
			const duration = st('blue', `(${Math.round(result.duration_ms)}ms)`);
			const label = st('gray', `  ${result.repo_name}`);
			const out = result.stdout.trim();
			const out_lines = out ? out.split('\n') : [];
			if (out_lines.length <= 1) {
				// single-line (or empty) output inline after the repo name
				log.info(out ? `${label} ${out} ${duration}` : `${label} ${duration}`);
			} else {
				// multi-line output indented under the repo
				log.info(`${label} ${duration}`);
				for (const line of out_lines) {
					log.info(st('gray', `    ${line}`));
				}
			}
		}
	}

	// Show failures with details
	if (failures.length > 0) {
		log.info(''); // blank line
		log.error(st('red', `✗ ${failures.length} failed:`));
		for (const result of failures) {
			const duration = `${Math.round(result.duration_ms)}ms`;
			log.error(st('gray', `  ${result.repo_name} ${st('blue', `(${duration})`)}`));

			if (result.error) {
				log.error(st('gray', `    Error: ${result.error}`));
			} else if (result.signal) {
				log.error(st('gray', `    Killed by ${result.signal}`));
			} else if (result.exit_code !== null) {
				log.error(st('gray', `    Exit code: ${result.exit_code}`));
			}

			if (result.stderr) {
				// Show first few lines of stderr
				const stderr_lines = result.stderr.trim().split('\n');
				const preview_lines = stderr_lines.slice(0, 3);
				for (const line of preview_lines) {
					log.error(st('gray', `    ${line}`));
				}
				if (stderr_lines.length > 3) {
					log.error(st('gray', `    ... (${stderr_lines.length - 3} more lines)`));
				}
			}
		}
	}

	// Summary
	log.info(''); // blank line
	const success_rate = ((successes.length / total) * 100).toFixed(0);
	const duration = `${Math.round(total_duration_ms)}ms`;

	if (failures.length === 0) {
		log.info(
			st('green', `✓ All ${total} repos succeeded in ${duration} (${success_rate}% success rate)`)
		);
	} else {
		log.info(
			st(
				'yellow',
				`⚠ ${successes.length}/${total} repos succeeded in ${duration} (${success_rate}% success rate)`
			)
		);
	}
};
