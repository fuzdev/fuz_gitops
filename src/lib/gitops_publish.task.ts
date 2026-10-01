import type { Task } from '@fuzdev/gro';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { z } from 'zod';
import { createInterface } from 'node:readline/promises';
import { styleText as st } from 'node:util';

import {
	gate_publish_readiness,
	get_gitops_ready,
	log_readiness_block,
	type ResolveGitopsReposOptions
} from './gitops_task_helpers.ts';
import type { LocalRepo } from './local_repo.ts';
import type { GitopsOperations, ReposOperations } from './operations.ts';
import { default_gitops_operations, default_repos_operations } from './operations_defaults.ts';
import {
	execute_publishing_plan,
	type PublishingOptions,
	type PublishingResult
} from './multi_repo_publisher.ts';
import {
	mask_secrets,
	masking_handler,
	redact_secrets,
	stdout_handler
} from './publishing_event_handler.ts';
import { generate_publishing_plan } from './publishing_plan.ts';
import { log_publishing_plan } from './publishing_plan_logging.ts';
import { derive_publish_steps, format_publish_steps, type PublishStep } from './publish_steps.ts';
import { decide_publish_gate, publish_run_failed } from './publish_gate.ts';
import {
	format_and_output,
	output_is_machine,
	route_human_output,
	type OutputFormatters
} from './output_helpers.ts';
import { GITOPS_CONFIG_PATH_DEFAULT, GITOPS_NPM_WAIT_TIMEOUT_DEFAULT } from './gitops_constants.ts';

/** @nodocs */
export const Args = z.strictObject({
	config: z
		.string()
		.meta({ description: 'path to the gitops config file, absolute or relative to the cwd' })
		.default(GITOPS_CONFIG_PATH_DEFAULT),
	registry: z
		.string()
		.meta({
			description:
				'path to the repos.toml registry, when `repos` would not find it walking up from the cwd'
		})
		.optional(),
	peer_strategy: z
		.enum(['exact', 'caret', 'tilde', 'gte'])
		.meta({
			description:
				'range prefix for a rewritten dependency of any type whose range has none (an existing prefix is kept; a wildcard becomes ^)'
		})
		.default('caret' as const),
	wetrun: z.boolean().meta({ description: 'actually publish (default is dry run)' }).default(false),
	format: z
		.enum(['stdout', 'json', 'markdown'])
		.meta({ description: 'output format' })
		.default('stdout'),
	deploy: z
		.boolean()
		.meta({
			description:
				'deploy each repo the run changed (published, or any dependency updated) after publishing'
		})
		.default(false),
	plan: z
		.boolean()
		.meta({ description: 'show the plan and confirm before publishing; --no-plan to skip' })
		.default(true),
	max_wait: z
		.number()
		.meta({ description: 'max time to wait for npm propagation in ms' })
		.default(GITOPS_NPM_WAIT_TIMEOUT_DEFAULT),
	emit_json: z
		.boolean()
		.meta({ description: 'stream structured publishing events as JSON-lines to stdout' })
		.default(false),
	outfile: z.string().meta({ description: 'write output to file instead of logging' }).optional(),
	verbose: z
		.boolean()
		.meta({ description: 'show additional details in plan output' })
		.default(false),
	preview: z
		.boolean()
		.meta({ description: 'show the ordered side-effects a --wetrun would perform' })
		.default(false)
});
export type Args = z.infer<typeof Args>;

/** @nodocs */
export const task: Task<Args> = {
	summary:
		'publish the packages the plan publishes, in dependency order (a dry run unless --wetrun)',
	Args,
	run: async ({ args, log }): Promise<void> => {
		const outcome = await run_gitops_publish(args, log);
		if (outcome === 'cancelled') {
			process.exit(0);
		} else if (outcome === 'failed') {
			process.exit(1);
		}
	}
};

/**
 * The side effects `run_gitops_publish` reaches through, injectable so the
 * order of its steps is testable.
 *
 * @nodocs
 */
export interface GitopsPublishDeps {
	/** Loads the configured repos as they sit (`get_gitops_ready`). */
	load_repos: (options: ResolveGitopsReposOptions) => Promise<{ local_repos: Array<LocalRepo> }>;
	/** Runs `repos status` for the readiness gate. */
	repos_ops: ReposOperations;
	/** The executor's operations. */
	ops: GitopsOperations;
	/**
	 * Asks the operator to confirm the plan, on stderr, so a machine-readable
	 * stdout carries nothing but its document or events.
	 */
	confirm: () => Promise<boolean>;
}

const default_gitops_publish_deps: GitopsPublishDeps = {
	load_repos: get_gitops_ready,
	repos_ops: default_repos_operations,
	ops: default_gitops_operations,
	confirm: async () => {
		process.stderr.write('Continue with publishing? (y/n): ');
		return prompt_for_confirmation();
	}
};

/**
 * Runs `gro gitops_publish`, in order: load the repos as they sit,
 * generate the plan, and — for `--wetrun` — run the readiness gate
 * (`gate_publish_readiness`, read-only: `repos status --fetch`) before
 * printing the plan and asking to confirm, so a refusal or a "no" changes
 * nothing. Then preflight and the executor. A dry run skips the gate and
 * prints the diagnostics' readiness block instead. Under `--emit_json`, or
 * `--format json` or `markdown` without `--outfile`, the log goes to stderr
 * and stdout carries the events or the report alone (`route_human_output`).
 *
 * @returns `cancelled` when the operator declines, `failed` when the run failed (its output already written), else `done`
 * @throws {TaskError} when the readiness gate refuses or loading fails
 * @nodocs
 */
export const run_gitops_publish = async (
	args: Args,
	log: Logger,
	deps: Partial<GitopsPublishDeps> = {}
): Promise<'done' | 'cancelled' | 'failed'> => {
	const { load_repos, repos_ops, ops, confirm } = { ...default_gitops_publish_deps, ...deps };
	const {
		config,
		registry,
		peer_strategy,
		wetrun,
		format,
		deploy,
		plan,
		max_wait,
		emit_json,
		outfile,
		verbose,
		preview
	} = args;

	// With a machine-readable stdout, every human line goes to stderr: the log (the plan, the
	// readiness block and gate, the executor's progress) and, in `confirm`, the prompt
	const write_stdout = route_human_output(log, to_child_stdout(args) === 'stderr');

	// Load repos as they sit; a real publish gates on their state below rather than moving them
	const { local_repos: repos } = await load_repos({ config, registry, log });

	// Generate the plan once; the executor consumes this exact plan (no second pass).
	const publishing_plan = await generate_publishing_plan(repos, { verbose, ops: ops.changeset });
	const preview_steps = preview ? derive_publish_steps(publishing_plan, { deploy }) : null;

	// Decide whether to show the plan + confirm, block, or proceed (the decision table lives
	// in `decide_publish_gate`; readline + exit stay at the edge).
	const gate = decide_publish_gate({ wetrun, show_plan: plan, plan: publishing_plan });

	if (wetrun) {
		// The readiness gate: fetch, then refuse unless every npm repo is ready. Read-only, and
		// before the prompt and any side effect. A plan with errors is refused without fetching:
		// below when the plan is shown, else by the executor before anything else it does.
		if (publishing_plan.errors.length === 0) {
			await gate_publish_readiness({
				local_repos: repos,
				registry,
				publishing: new Set(publishing_plan.version_changes.map((vc) => vc.package_name)),
				log,
				repos_ops
			});
		}
	} else {
		log_readiness_block(repos, log);
	}

	// A real publish that shows its plan prints it first — including before a `blocked` throw,
	// so the operator sees the errors that blocked it.
	if (gate.action !== 'proceed') {
		log.info(st('cyan', 'Publishing Plan'));
		log_publishing_plan(publishing_plan, log, { verbose });
	}

	if (gate.action === 'blocked') {
		throw new Error(gate.message);
	} else if (gate.action === 'confirm') {
		if (preview_steps) log_preview(preview_steps, log);

		// Ask for confirmation
		log.info(st('yellow', '⚠️  This will publish the packages shown above.'));
		const confirmed = await confirm();
		if (!confirmed) {
			log.info('Publishing cancelled');
			return 'cancelled';
		}
	} else if (preview_steps && format === 'stdout') {
		// proceed (dry run or --no-plan): only render to stdout for the human format;
		// json/markdown carry the preview in their structured output, so logging here too
		// would corrupt that stream.
		log_preview(preview_steps, log);
	}

	// Publishing options
	const options: PublishingOptions = {
		wetrun,
		version_strategy: peer_strategy,
		deploy,
		max_wait,
		log,
		ops,
		registry,
		// Live JSON-lines stream when requested, secrets masked since failure messages carry
		// npm's stderr; events also surface on the result.
		events: emit_json ? masking_handler(stdout_handler(write_stdout)) : undefined,
		child_stdout: to_child_stdout(args)
	};

	// Execute publishing (may throw on fatal errors like circular dependencies)
	let result: PublishingResult;
	let fatal_error: Error | null = null;

	try {
		result = await execute_publishing_plan(repos, publishing_plan, options);
	} catch (error) {
		// Construct a failure result for fatal errors so output can still be generated
		fatal_error = error instanceof Error ? error : new Error(String(error));
		result = {
			ok: false,
			published: [],
			// FATAL_ERROR is a placeholder name: markdown shows only the message, the JSON report shows the entry
			failed: [{ name: 'FATAL_ERROR', error: fatal_error }],
			duration: 0,
			events: [],
			summary: { total: 0, published: 0, failed: 1, skipped: 0, duration: 0 },
			plan_errors: publishing_plan.errors,
			plan_warnings: publishing_plan.warnings
		};
	}

	// Format and output result (always runs, even on fatal errors)
	// Note: stdout format is handled by the executor's logging
	if (format !== 'stdout') {
		await format_and_output({ result, fatal_error, preview_steps }, create_publish_formatters(), {
			format,
			outfile,
			log,
			write_stdout
		});
	}

	return publish_run_failed(result, fatal_error) ? 'failed' : 'done';
};

/**
 * Where `gro publish` and `gro deploy`, streaming live, show their stdout: on
 * our stderr when our stdout carries a machine-readable stream — the
 * `--emit_json` events, or a JSON or markdown report not sent to `--outfile` —
 * so their output can't corrupt it, else on our stdout. The same test sends
 * the task's own log and prompt to stderr.
 *
 * @nodocs
 */
export const to_child_stdout = (
	args: Pick<Args, 'emit_json' | 'format' | 'outfile'>
): 'stdout' | 'stderr' =>
	args.emit_json || output_is_machine(args.format, args.outfile) ? 'stderr' : 'stdout';

/**
 * A failed package's markdown: the message's first line on the bullet, and the
 * rest — the end of a command's stderr — in an indented fenced block, its fence
 * longer than any backtick run inside so stderr can't close it.
 *
 * @param name - the package name
 * @param message - the failure message
 * @nodocs
 */
export const format_failure_markdown = (name: string, message: string): Array<string> => {
	const [head, ...rest] = message.split('\n');
	const lines = [`- \`${name}\`: ${head}`];
	if (rest.length === 0) return lines;
	const longest_run = Math.max(0, ...(rest.join('\n').match(/`+/g) ?? []).map((r) => r.length));
	const fence = '`'.repeat(Math.max(3, longest_run + 1));
	lines.push('', `  ${fence}`, ...rest.map((l) => `  ${l}`), `  ${fence}`);
	return lines;
};

/** @nodocs */
export interface PublishResultData {
	result: PublishingResult;
	fatal_error: Error | null;
	preview_steps: Array<PublishStep> | null;
}

/**
 * The `--format json` document: the result with each failure's `Error` as its
 * message string — an `Error` serializes as `{}` — and secrets masked in those
 * messages and in the events, as the markdown report and the live stream mask
 * them. A fatal error is the one `FATAL_ERROR` entry in `failed`, carrying its
 * message. `preview` is present under `--preview`.
 *
 * @nodocs
 */
export const to_publish_result_json = (
	data: PublishResultData
): Omit<PublishingResult, 'failed'> & {
	failed: Array<{ name: string; error: string }>;
	preview?: Array<PublishStep>;
} => {
	const result = {
		...data.result,
		failed: data.result.failed.map(({ name, error }) => ({
			name,
			error: redact_secrets(error.message)
		})),
		// the result's events are the executor's unmasked capture; mask them as the live stream is
		events: data.result.events.map(mask_secrets)
	};
	return data.preview_steps ? { ...result, preview: data.preview_steps } : result;
};

const create_publish_formatters = (): OutputFormatters<PublishResultData> => ({
	json: (data) => JSON.stringify(to_publish_result_json(data), null, 2),
	markdown: (data) => format_result_markdown(data.result, data.fatal_error, data.preview_steps),
	stdout: () => {
		// stdout format is handled by the executor's logging
		// This should never be called due to early return in task
	}
});

/** Logs the ordered side-effect preview. */
const log_preview = (steps: Array<PublishStep>, log: Logger): void => {
	log.info(st('cyan', '\nSide-effect preview (what a real publish would perform):'));
	for (const line of format_publish_steps(steps)) {
		log.info(st('dim', `  ${line}`));
	}
};

// Format the publishing result as markdown
const format_result_markdown = (
	result: PublishingResult,
	fatal_error: Error | null,
	preview_steps: Array<PublishStep> | null
): Array<string> => {
	const lines: Array<string> = [];

	lines.push('# Publishing Result');
	lines.push('');

	// Show fatal error prominently if present
	if (fatal_error) {
		lines.push('## ❌ Fatal Error');
		lines.push('');
		lines.push(`**Error**: ${redact_secrets(fatal_error.message)}`);
		lines.push('');
		lines.push('Publishing could not proceed due to the error above.');
		lines.push('');
		return lines;
	}

	lines.push(`**Status**: ${result.ok ? '✅ Success' : '❌ Failed'}`);
	lines.push(`**Duration**: ${(result.duration / 1000).toFixed(1)}s`);
	lines.push(`**Published**: ${result.published.length} packages`);

	if (result.failed.length > 0) {
		lines.push(`**Failed**: ${result.failed.length} packages`);
	}

	if (result.published.length > 0) {
		lines.push('');
		lines.push('## Published Packages');
		lines.push('');
		for (const pkg of result.published) {
			lines.push(`- \`${pkg.name}\`: ${pkg.old_version} → ${pkg.new_version}`);
		}
	}

	if (result.failed.length > 0) {
		lines.push('');
		lines.push('## Failed Packages');
		lines.push('');
		for (const { name, error } of result.failed) {
			lines.push(...format_failure_markdown(name, redact_secrets(error.message)));
		}
	}

	if (result.plan_warnings.length > 0) {
		lines.push('');
		lines.push('## Plan Warnings');
		lines.push('');
		for (const warning of result.plan_warnings) lines.push(`- ${warning}`);
	}

	if (result.plan_errors.length > 0) {
		lines.push('');
		lines.push('## Plan Errors');
		lines.push('');
		for (const plan_error of result.plan_errors) lines.push(`- ${plan_error}`);
	}

	if (preview_steps) {
		lines.push('');
		lines.push('## Side-Effect Preview');
		lines.push('');
		lines.push('```');
		for (const line of format_publish_steps(preview_steps)) lines.push(line);
		lines.push('```');
	}

	return lines;
};

/**
 * Prompts user for y/n confirmation.
 * Returns true if user enters 'y', false otherwise.
 */
const prompt_for_confirmation = async (): Promise<boolean> => {
	const rl = createInterface({
		input: process.stdin,
		output: process.stderr
	});

	const answer = await rl.question('');
	rl.close();

	return answer.toLowerCase() === 'y';
};
