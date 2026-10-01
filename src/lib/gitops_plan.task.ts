import type { Task } from '@fuzdev/gro';
import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { z } from 'zod';
import { styleText as st } from 'node:util';

import {
	get_gitops_ready,
	log_readiness_block,
	type ResolveGitopsReposOptions
} from './gitops_task_helpers.ts';
import type { LocalRepo } from './local_repo.ts';
import type { ChangesetOperations } from './operations.ts';
import { default_changeset_operations } from './operations_defaults.ts';
import {
	generate_publishing_plan,
	version_change_kind,
	type PublishingPlan
} from './publishing_plan.ts';
import { log_publishing_plan, type LogPlanOptions } from './publishing_plan_logging.ts';
import {
	format_and_output,
	output_is_machine,
	route_human_output,
	type OutputFormatters
} from './output_helpers.ts';
import { GITOPS_CONFIG_PATH_DEFAULT } from './gitops_constants.ts';

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
	format: z
		.enum(['stdout', 'json', 'markdown'])
		.meta({ description: 'output format' })
		.default('stdout'),
	outfile: z.string().meta({ description: 'write output to file instead of logging' }).optional(),
	verbose: z.boolean().meta({ description: 'show additional details' }).default(false)
});
export type Args = z.infer<typeof Args>;

/**
 * Generate a publishing plan showing what would happen during multi-repo publishing.
 * Shows version changes, dependency updates, and breaking change cascades.
 *
 * Usage:
 *   `gro gitops_plan`
 *   `gro gitops_plan --registry ../repos.toml`
 *   `gro gitops_plan --config ./custom.config.ts`
 *
 * @nodocs
 */
export const task: Task<Args> = {
	summary: 'generate a publishing plan based on changesets',
	Args,
	run: async ({ args, log }): Promise<void> => {
		await run_gitops_plan(args, log);
	}
};

/**
 * The side effects `run_gitops_plan` reaches through, injectable for tests.
 *
 * @nodocs
 */
export interface GitopsPlanDeps {
	/** Loads the configured repos as they sit (`get_gitops_ready`). */
	load_repos: (options: ResolveGitopsReposOptions) => Promise<{ local_repos: Array<LocalRepo> }>;
	/** Reads each repo's changesets for the plan. */
	changeset_ops: ChangesetOperations;
}

const default_gitops_plan_deps: GitopsPlanDeps = {
	load_repos: get_gitops_ready,
	changeset_ops: default_changeset_operations
};

/**
 * Runs `gro gitops_plan`: loads the repos as they sit, logs the readiness
 * block, generates the plan, and outputs it. Under `--format json` or
 * `markdown` without `--outfile`, the log goes to stderr and stdout carries
 * the document alone (`route_human_output`).
 *
 * @throws {Error} when the plan has errors that would block publishing, after outputting it
 * @nodocs
 */
export const run_gitops_plan = async (
	args: Args,
	log: Logger,
	deps: Partial<GitopsPlanDeps> = {}
): Promise<void> => {
	const { load_repos, changeset_ops } = { ...default_gitops_plan_deps, ...deps };
	const { config, registry, format, outfile, verbose } = args;
	const write_stdout = route_human_output(log, output_is_machine(format, outfile));

	log.info(st('cyan', 'Generating multi-repo publishing plan...'));

	// Load local repos as they sit, and say which aren't at rest
	const { local_repos } = await load_repos({ config, registry, log });
	log_readiness_block(local_repos, log);

	log.info(`  Found ${local_repos.length} local repos`);

	// Generate publishing plan
	const plan = await generate_publishing_plan(local_repos, { log, verbose, ops: changeset_ops });

	// Format and output using output_helpers
	await format_and_output(plan, create_plan_formatters({ verbose }), {
		format,
		outfile,
		log,
		write_stdout
	});

	// Exit with error if there are blocking issues
	if (plan.errors.length > 0) {
		throw new Error('Publishing plan found errors that would block publishing');
	}
};

const create_plan_formatters = (
	options: LogPlanOptions = {}
): OutputFormatters<PublishingPlan> => ({
	json: (plan) => {
		const output = {
			publishing_order: plan.publishing_order,
			version_changes: plan.version_changes,
			dependency_updates: plan.dependency_updates,
			breaking_cascades: Object.fromEntries(plan.breaking_cascades),
			warnings: plan.warnings,
			info: plan.info,
			no_changes: plan.no_changes,
			errors: plan.errors
		};
		return JSON.stringify(output, null, 2);
	},
	markdown: (plan) => format_plan_as_markdown(plan),
	stdout: (plan, log) => log_publishing_plan(plan, log, options)
});

const format_plan_as_markdown = (plan: PublishingPlan): Array<string> => {
	const lines: Array<string> = [];
	const {
		publishing_order,
		version_changes,
		dependency_updates,
		breaking_cascades,
		warnings,
		info,
		no_changes,
		errors
	} = plan;

	lines.push('# Publishing Plan');
	lines.push('');

	// Errors
	if (errors.length > 0) {
		lines.push('## ❌ Errors');
		lines.push('');
		for (const error of errors) {
			lines.push(`- ${error}`);
		}
		lines.push('');
	}

	// Publishing order
	if (publishing_order.length > 0) {
		lines.push('## Publishing Order');
		lines.push('');
		lines.push(publishing_order.map((p) => `\`${p}\``).join(' → '));
		lines.push('');
	}

	// Version changes
	if (version_changes.length > 0) {
		const with_changesets = version_changes.filter((vc) => version_change_kind(vc) === 'explicit');
		const with_escalation = version_changes.filter(
			(vc) => version_change_kind(vc) === 'escalation'
		);
		const with_auto_changesets = version_changes.filter((vc) => version_change_kind(vc) === 'auto');

		if (with_changesets.length > 0) {
			lines.push('## Version Changes (from changesets)');
			lines.push('');
			lines.push('| Package | From | To | Bump | Breaking |');
			lines.push('|---------|------|----|------|----------|');
			for (const change of with_changesets) {
				const breaking = change.breaking ? '💥 Yes' : 'No';
				lines.push(
					`| \`${change.package_name}\` | ${change.from} | ${change.to} | ${change.bump_type} | ${breaking} |`
				);
			}
			lines.push('');
		}

		if (with_escalation.length > 0) {
			lines.push('## Version Changes (bump escalation required)');
			lines.push('');
			lines.push('| Package | From | To | Changesets Bump | Required Bump | Breaking |');
			lines.push('|---------|------|-----|-----------------|---------------|----------|');
			for (const change of with_escalation) {
				const breaking = change.breaking ? '💥 Yes' : 'No';
				lines.push(
					`| \`${change.package_name}\` | ${change.from} | ${change.to} | ${change.existing_bump} | ${change.required_bump} | ${breaking} |`
				);
			}
			lines.push('');
			lines.push(
				'> ⬆️ These packages have changesets, but dependencies require a larger version bump.'
			);
			lines.push('');
		}

		if (with_auto_changesets.length > 0) {
			lines.push('## Version Changes (auto-generated for dependency updates)');
			lines.push('');
			lines.push('| Package | From | To | Bump | Breaking |');
			lines.push('|---------|------|-----|------|----------|');
			for (const change of with_auto_changesets) {
				const breaking = change.breaking ? '💥 Yes' : 'No';
				lines.push(
					`| \`${change.package_name}\` | ${change.from} | ${change.to} | ${change.bump_type} | ${breaking} |`
				);
			}
			lines.push('');
		}
	} else {
		lines.push('## No Packages to Publish');
		lines.push('');
		lines.push('No packages have changesets to publish.');
		lines.push('');
	}

	// Dependency cascades
	if (breaking_cascades.size > 0) {
		lines.push('## Dependency Cascades');
		lines.push('');
		for (const [pkg, affected] of breaking_cascades) {
			lines.push(`- \`${pkg}\` affects: ${affected.map((a) => `\`${a}\``).join(', ')}`);
		}
		lines.push('');
	}

	// Dependency updates - show diff-style
	if (dependency_updates.length > 0) {
		// Group by package
		const updates_by_package: Map<string, typeof dependency_updates> = new Map();
		for (const update of dependency_updates) {
			const updates = updates_by_package.get(update.dependent_package) || [];
			updates.push(update);
			updates_by_package.set(update.dependent_package, updates);
		}

		lines.push('## Dependency Updates');
		lines.push('');
		for (const [pkg, updates] of updates_by_package) {
			const has_version_change = version_changes.some((vc) => vc.package_name === pkg);
			const label = has_version_change ? '' : ' (no republish)';
			lines.push(`### ${pkg}${label}`);
			lines.push('');
			lines.push('```diff');
			for (const update of updates) {
				const type_label =
					update.type === 'dependencies'
						? 'prod'
						: update.type === 'peerDependencies'
							? 'peer'
							: 'dev';
				lines.push(
					`- "${update.updated_dependency}": "${update.current_version}"  # ${type_label}`
				);
				lines.push(`+ "${update.updated_dependency}": "${update.new_version}"  # ${type_label}`);
			}
			lines.push('```');
			lines.push('');
		}
	}

	// Warnings (actual issues requiring attention)
	if (warnings.length > 0) {
		lines.push('## ⚠️ Warnings');
		lines.push('');
		lines.push('*Issues that require attention:*');
		lines.push('');
		for (const warning of warnings) {
			lines.push(`- ${warning}`);
		}
		lines.push('');
	}

	// Info (normal status, not warnings)
	if (info.length > 0) {
		lines.push('## ℹ️ Info');
		lines.push('');
		for (const line of info) {
			lines.push(`- ${line}`);
		}
		lines.push('');
	}

	// Packages with nothing to publish (normal status)
	if (no_changes.length > 0) {
		lines.push('## No Changes to Publish');
		lines.push('');
		lines.push('*These packages have no changesets and nothing to publish:*');
		lines.push('');
		for (const pkg of no_changes) {
			lines.push(`- \`${pkg}\``);
		}
		lines.push('');
	}

	// Summary
	const breaking_count = version_changes.filter((vc) => vc.breaking).length;
	lines.push('## Summary');
	lines.push('');
	lines.push(`- **Packages to publish**: ${version_changes.length}`);
	lines.push(`- **Dependency updates**: ${dependency_updates.length}`);
	lines.push(`- **Breaking changes**: ${breaking_count}`);
	lines.push(`- **Warnings**: ${warnings.length}`);
	lines.push(`- **Errors**: ${errors.length}`);

	return lines;
};
