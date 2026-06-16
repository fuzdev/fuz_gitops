import type {Task} from '@fuzdev/gro';
import type {Logger} from '@fuzdev/fuz_util/log.ts';
import {z} from 'zod';
import {createInterface} from 'node:readline/promises';
import {styleText as st} from 'node:util';

import {get_gitops_ready} from './gitops_task_helpers.ts';
import {
	execute_publishing_plan,
	type PublishingOptions,
	type PublishingResult,
} from './multi_repo_publisher.ts';
import {stdout_handler} from './publishing_event_handler.ts';
import {generate_publishing_plan, log_publishing_plan} from './publishing_plan.ts';
import {derive_publish_steps, format_publish_steps, type PublishStep} from './publish_steps.ts';
import {decide_publish_gate, publish_run_failed} from './publish_gate.ts';
import {format_and_output, type OutputFormatters} from './output_helpers.ts';
import {GITOPS_CONFIG_PATH_DEFAULT, GITOPS_NPM_WAIT_TIMEOUT_DEFAULT} from './gitops_constants.ts';

/** @nodocs */
export const Args = z.strictObject({
	config: z
		.string()
		.meta({description: 'path to the gitops config file, absolute or relative to the cwd'})
		.default(GITOPS_CONFIG_PATH_DEFAULT),
	dir: z
		.string()
		.meta({description: 'path containing the repos, defaults to the parent of the config dir'})
		.optional(),
	peer_strategy: z
		.enum(['exact', 'caret', 'tilde'])
		.meta({description: 'version strategy for peer dependencies'})
		.default('caret' as const),
	wetrun: z.boolean().meta({description: 'actually publish (default is dry run)'}).default(false),
	format: z
		.enum(['stdout', 'json', 'markdown'])
		.meta({description: 'output format'})
		.default('stdout'),
	deploy: z.boolean().meta({description: 'deploy all repos after publishing'}).default(false),
	plan: z
		.boolean()
		.meta({description: 'show the plan and confirm before publishing; --no-plan to skip'})
		.default(true),
	max_wait: z
		.number()
		.meta({description: 'max time to wait for npm propagation in ms'})
		.default(GITOPS_NPM_WAIT_TIMEOUT_DEFAULT),
	emit_json: z
		.boolean()
		.meta({description: 'stream structured publishing events as JSON-lines to stdout'})
		.default(false),
	outfile: z.string().meta({description: 'write output to file instead of logging'}).optional(),
	verbose: z.boolean().meta({description: 'show additional details in plan output'}).default(false),
	sync: z
		.boolean()
		.meta({
			description:
				'sync repos (switch branch, pull, install) before the dry run instead of reading the working tree as-is; always on for --wetrun',
		})
		.default(false),
	preview: z
		.boolean()
		.meta({description: 'show the ordered side-effects a --wetrun would perform'})
		.default(false),
});
export type Args = z.infer<typeof Args>;

/** @nodocs */
export const task: Task<Args> = {
	summary: 'publish all repos in dependency order',
	Args,
	run: async ({args, log}): Promise<void> => {
		const {
			config,
			dir,
			peer_strategy,
			wetrun,
			format,
			deploy,
			plan,
			max_wait,
			emit_json,
			outfile,
			verbose,
			sync,
			preview,
		} = args;

		// Load repos. A dry run reads the working tree as-is unless `--sync`;
		// a real publish (`--wetrun`) always syncs so preflight sees the canonical branches.
		const {local_repos: repos} = await get_gitops_ready({
			config,
			dir,
			download: false, // Don't download if missing
			sync: sync || wetrun,
			log,
		});

		// Generate the plan once; the executor consumes this exact plan (no second pass).
		const publishing_plan = await generate_publishing_plan(repos, {verbose});
		const preview_steps = preview ? derive_publish_steps(publishing_plan, {deploy}) : null;

		// Decide whether to show the plan + confirm, block, or proceed (the decision table lives
		// in `decide_publish_gate`; readline + exit stay here at the edge).
		const gate = decide_publish_gate({wetrun, show_plan: plan, plan: publishing_plan});

		// A real publish that shows its plan prints it first — including before a `blocked` throw,
		// so the operator sees the errors that blocked it.
		if (gate.action !== 'proceed') {
			log.info(st('cyan', 'Publishing Plan'));
			log_publishing_plan(publishing_plan, log, {verbose});
		}

		if (gate.action === 'blocked') {
			throw new Error(gate.message);
		} else if (gate.action === 'confirm') {
			if (preview_steps) log_preview(preview_steps, log);

			// Ask for confirmation
			log.info(st('yellow', '⚠️  This will publish the packages shown above.'));
			process.stdout.write('Continue with publishing? (y/n): ');
			const confirmed = await prompt_for_confirmation();
			if (!confirmed) {
				log.info('Publishing cancelled');
				process.exit(0);
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
			// Live JSON-lines stream when requested; events also surface on the result.
			events: emit_json ? stdout_handler() : undefined,
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
				// Note: FATAL_ERROR is a placeholder - only fatal_error.message is displayed in output
				failed: [{name: 'FATAL_ERROR', error: fatal_error}],
				duration: 0,
				events: [],
				summary: {total: 0, published: 0, failed: 1, skipped: 0, duration: 0},
				plan_errors: publishing_plan.errors,
				plan_warnings: publishing_plan.warnings,
			};
		}

		// Format and output result (always runs, even on fatal errors)
		// Note: stdout format is handled by the executor's logging
		if (format !== 'stdout') {
			await format_and_output({result, fatal_error, preview_steps}, create_publish_formatters(), {
				format,
				outfile,
				log,
			});
		}

		// Exit with error if failed
		if (publish_run_failed(result, fatal_error)) {
			process.exit(1);
		}
	},
};

interface PublishResultData {
	result: PublishingResult;
	fatal_error: Error | null;
	preview_steps: Array<PublishStep> | null;
}

const create_publish_formatters = (): OutputFormatters<PublishResultData> => ({
	json: (data) =>
		JSON.stringify(
			data.preview_steps ? {...data.result, preview: data.preview_steps} : data.result,
			null,
			2,
		),
	markdown: (data) => format_result_markdown(data.result, data.fatal_error, data.preview_steps),
	stdout: () => {
		// stdout format is handled by the executor's logging
		// This should never be called due to early return in task
	},
});

/** Logs the ordered side-effect preview to stdout. */
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
	preview_steps: Array<PublishStep> | null,
): Array<string> => {
	const lines: Array<string> = [];

	lines.push('# Publishing Result');
	lines.push('');

	// Show fatal error prominently if present
	if (fatal_error) {
		lines.push('## ❌ Fatal Error');
		lines.push('');
		lines.push(`**Error**: ${fatal_error.message}`);
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
		for (const {name, error} of result.failed) {
			lines.push(`- \`${name}\`: ${error.message}`);
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
		output: process.stdout,
	});

	const answer = await rl.question('');
	rl.close();

	return answer.toLowerCase() === 'y';
};
