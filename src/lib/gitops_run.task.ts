import {TaskError, type Task} from '@ryanatkn/gro';
import {z} from 'zod';
import {map_concurrent_settled} from '@fuzdev/fuz_util/async.js';
import {spawn_out} from '@fuzdev/fuz_util/process.js';
import {styleText as st} from 'node:util';
import {resolve} from 'node:path';

import {get_repo_paths} from './repo_ops.js';
import {GITOPS_CONCURRENCY_DEFAULT, GITOPS_CONFIG_PATH_DEFAULT} from './gitops_constants.js';

export const Args = z.strictObject({
	command: z.string().meta({description: 'shell command to run in each repo'}),
	config: z
		.string()
		.meta({description: 'path to the gitops config file'})
		.default(GITOPS_CONFIG_PATH_DEFAULT),
	concurrency: z
		.number()
		.int()
		.min(1)
		.meta({description: 'maximum number of repos to run in parallel'})
		.default(GITOPS_CONCURRENCY_DEFAULT),
	format: z.enum(['text', 'json']).meta({description: 'output format'}).default('text'),
});
export type Args = z.infer<typeof Args>;

interface RunResult {
	repo_name: string;
	repo_dir: string;
	status: 'success' | 'failure';
	exit_code: number;
	stdout: string;
	stderr: string;
	duration_ms: number;
	error?: string;
}

export const task: Task<Args> = {
	Args,
	summary: 'run a shell command across all repos in parallel',
	run: async ({args, log}) => {
		const {command, config, concurrency, format} = args;

		// Get repo paths (lightweight, no library.ts loading needed)
		const config_path = resolve(config);
		const repos = await get_repo_paths(config_path);

		if (repos.length === 0) {
			throw new TaskError('No repos found in config');
		}

		log.info(
			`Running ${st('cyan', command)} across ${repos.length} repos (concurrency: ${concurrency})`,
		);

		const start_time = performance.now();

		// Run command in parallel across all repos
		const results = await map_concurrent_settled(
			repos,
			async (repo) => {
				const repo_start = performance.now();
				const repo_name = repo.name;
				const repo_dir = repo.path;

				try {
					// Parse command into cmd + args for spawn
					// For now, we use shell mode to support pipes/redirects/etc
					const spawned = await spawn_out('sh', ['-c', command], {
						cwd: repo_dir,
					});

					const duration_ms = performance.now() - repo_start;
					const success = spawned.result.ok;

					const result: RunResult = {
						repo_name,
						repo_dir,
						status: success ? 'success' : 'failure',
						exit_code: spawned.result.code ?? 0,
						stdout: spawned.stdout || '',
						stderr: spawned.stderr || '',
						duration_ms,
					};

					return result;
				} catch (error) {
					const duration_ms = performance.now() - repo_start;
					return {
						repo_name,
						repo_dir,
						status: 'failure' as const,
						exit_code: -1,
						stdout: '',
						stderr: '',
						duration_ms,
						error: String(error),
					};
				}
			},
			concurrency,
		);

		const total_duration_ms = performance.now() - start_time;

		// Process results
		const successes: Array<RunResult> = [];
		const failures: Array<RunResult> = [];

		for (const result of results) {
			if (result.status === 'fulfilled') {
				const run_result = result.value;
				if (run_result.status === 'success') {
					successes.push(run_result);
				} else {
					failures.push(run_result);
				}
			} else {
				// This shouldn't happen since we catch errors in the task fn
				// but handle it anyway
				failures.push({
					repo_name: 'unknown',
					repo_dir: 'unknown',
					status: 'failure',
					exit_code: -1,
					stdout: '',
					stderr: '',
					duration_ms: 0,
					error: String(result.reason),
				});
			}
		}

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
					duration_ms: Math.round(total_duration_ms),
				},
			};
			// eslint-disable-next-line no-console
			console.log(JSON.stringify(json_output, null, 2));
		} else {
			// Text format
			log.info(''); // blank line

			// Show successes
			if (successes.length > 0) {
				log.info(st('green', `✓ ${successes.length} succeeded:`));
				for (const result of successes) {
					const duration = `${Math.round(result.duration_ms)}ms`;
					log.info(st('gray', `  ${result.repo_name} ${st('blue', `(${duration})`)}`));
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
					} else if (result.exit_code !== 0) {
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
			const total = repos.length;
			const success_rate = ((successes.length / total) * 100).toFixed(0);
			const duration = `${Math.round(total_duration_ms)}ms`;

			if (failures.length === 0) {
				log.info(
					st(
						'green',
						`✓ All ${total} repos succeeded in ${duration} (${success_rate}% success rate)`,
					),
				);
			} else {
				log.info(
					st(
						'yellow',
						`⚠ ${successes.length}/${total} repos succeeded in ${duration} (${success_rate}% success rate)`,
					),
				);
			}
		}

		// Exit with error if any failures (so CI fails)
		if (failures.length > 0) {
			throw new TaskError(`${failures.length} repos failed`);
		}
	},
};
