import type {Task} from '@ryanatkn/gro';
import {z} from 'zod';
import {createInterface} from 'node:readline/promises';
import {styleText as st} from 'node:util';

import {get_gitops_ready} from './gitops_task_helpers.js';
import {
	publish_repos,
	type PublishingOptions,
	type PublishingResult,
} from './multi_repo_publisher.js';
import {generate_publishing_plan, log_publishing_plan} from './publishing_plan.js';
import {format_and_output, type OutputFormatters} from './output_helpers.js';
import {
	GITOPS_CONFIG_PATH_DEFAULT,
	GITOPS_NPM_WAIT_TIMEOUT_DEFAULT,
} from './gitops_constants.js';

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
	dry_run: z
		.boolean()
		.meta({description: 'perform a dry run without actually publishing'})
		.default(false),
	format: z
		.enum(['stdout', 'json', 'markdown'])
		.meta({description: 'output format'})
		.default('stdout'),
	deploy: z.boolean().meta({description: 'deploy all repos after publishing'}).default(false),
	plan: z.boolean().meta({description: 'dual of no-plan'}).default(true),
	'no-plan': z
		.boolean()
		.meta({description: 'skip plan confirmation before publishing'})
		.default(false),
	max_wait: z
		.number()
		.meta({description: 'max time to wait for npm propagation in ms'})
		.default(GITOPS_NPM_WAIT_TIMEOUT_DEFAULT),
	skip_install: z
		.boolean()
		.meta({description: 'skip npm install after dependency updates'})
		.default(false),
	outfile: z.string().meta({description: 'write output to file instead of logging'}).optional(),
	verbose: z.boolean().meta({description: 'show additional details in plan output'}).default(false),
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
			dry_run,
			format,
			deploy,
			plan,
			max_wait,
			skip_install,
			outfile,
			verbose,
		} = args;

		// Load repos
		const {local_repos: repos} = await get_gitops_ready({
			config,
			dir,
			download: false, // Don't download if missing
			log,
		});

		// Show plan if requested (skip for dry runs)
		if (plan && !dry_run) {
			log.info(st('cyan', 'Publishing Plan'));
			const plan_result = await generate_publishing_plan(repos, {log, verbose});
			log_publishing_plan(plan_result, log, {verbose});

			if (plan_result.errors.length > 0) {
				throw new Error('Cannot proceed with publishing due to errors');
			}

			// Ask for confirmation
			log.info(st('yellow', '⚠️  This will publish the packages shown above.'));
			process.stdout.write('Continue with publishing? (y/n): ');
			const confirmed = await prompt_for_confirmation();
			if (!confirmed) {
				log.info('Publishing cancelled');
				process.exit(0);
			}
		}

		// Publishing options
		const options: PublishingOptions = {
			dry_run,
			update_deps: true, // Always update dependencies
			version_strategy: peer_strategy,
			deploy,
			max_wait,
			skip_install,
			log,
		};

		// Execute publishing (may throw on fatal errors like circular dependencies)
		let result: PublishingResult;
		let fatal_error: Error | null = null;

		try {
			result = await publish_repos(repos, options);
		} catch (error) {
			// Construct a failure result for fatal errors so output can still be generated
			fatal_error = error instanceof Error ? error : new Error(String(error));
			result = {
				ok: false,
				published: [],
				// Note: FATAL_ERROR is a placeholder - only fatal_error.message is displayed in output
				failed: [{name: 'FATAL_ERROR', error: fatal_error}],
				duration: 0,
			};
		}

		// Format and output result (always runs, even on fatal errors)
		// Note: stdout format is handled by publish_repos function's logging
		if (format !== 'stdout') {
			await format_and_output({result, fatal_error}, create_publish_formatters(), {
				format,
				outfile,
				log,
			});
		}

		// Exit with error if failed
		if (!result.ok || fatal_error) {
			process.exit(1);
		}
	},
};

interface PublishResultData {
	result: PublishingResult;
	fatal_error: Error | null;
}

const create_publish_formatters = (): OutputFormatters<PublishResultData> => ({
	json: (data) => JSON.stringify(data.result, null, 2),
	markdown: (data) => format_result_markdown(data.result, data.fatal_error),
	stdout: () => {
		// stdout format is handled by publish_repos function's logging
		// This should never be called due to early return in task
	},
});

// Format the publishing result as markdown
const format_result_markdown = (
	result: PublishingResult,
	fatal_error: Error | null,
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
