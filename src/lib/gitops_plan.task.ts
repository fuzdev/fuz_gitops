import type {Task} from '@ryanatkn/gro';
import {z} from 'zod';
import {styleText as st} from 'node:util';

import {get_gitops_ready} from './gitops_task_helpers.js';
import {
	generate_publishing_plan,
	log_publishing_plan,
	type PublishingPlan,
	type LogPlanOptions,
} from './publishing_plan.js';
import {format_and_output, type OutputFormatters} from './output_helpers.js';
import {GITOPS_CONFIG_PATH_DEFAULT} from './gitops_constants.js';

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
	format: z
		.enum(['stdout', 'json', 'markdown'])
		.meta({description: 'output format'})
		.default('stdout'),
	outfile: z.string().meta({description: 'write output to file instead of logging'}).optional(),
	verbose: z.boolean().meta({description: 'show additional details'}).default(false),
});
export type Args = z.infer<typeof Args>;

/**
 * Generate a publishing plan showing what would happen during multi-repo publishing.
 * Shows version changes, dependency updates, and breaking change cascades.
 *
 * Usage:
 *   gro gitops_plan
 *   gro gitops_plan --dir ../repos
 *   gro gitops_plan --config ./custom.config.ts
 *
 * @nodocs
 */
export const task: Task<Args> = {
	summary: 'generate a publishing plan based on changesets',
	Args,
	run: async ({args, log}): Promise<void> => {
		const {dir, config, format, outfile, verbose} = args;

		log.info(st('cyan', 'Generating multi-repo publishing plan...'));

		// Load local repos
		const {local_repos} = await get_gitops_ready({
			config,
			dir,
			download: false, // Don't download if missing
			log,
		});

		if (local_repos.length === 0) {
			log.error('No local repos found');
			return;
		}

		log.info(`  Found ${local_repos.length} local repos`);

		// Generate publishing plan
		const plan = await generate_publishing_plan(local_repos, {log, verbose});

		// Format and output using output_helpers
		await format_and_output(plan, create_plan_formatters({verbose}), {format, outfile, log});

		// Exit with error if there are blocking issues
		if (plan.errors.length > 0) {
			throw new Error('Publishing plan found errors that would block publishing');
		}
	},
};

const create_plan_formatters = (
	options: LogPlanOptions = {},
): OutputFormatters<PublishingPlan> => ({
	json: (plan) => {
		const output = {
			publishing_order: plan.publishing_order,
			version_changes: plan.version_changes,
			dependency_updates: plan.dependency_updates,
			breaking_cascades: Object.fromEntries(plan.breaking_cascades),
			warnings: plan.warnings,
			info: plan.info,
			errors: plan.errors,
		};
		return JSON.stringify(output, null, 2);
	},
	markdown: (plan) => format_plan_as_markdown(plan),
	stdout: (plan, log) => log_publishing_plan(plan, log, options),
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
		errors,
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
		const with_changesets = version_changes.filter(
			(vc) => vc.has_changesets && !vc.needs_bump_escalation,
		);
		const with_escalation = version_changes.filter((vc) => vc.needs_bump_escalation);
		const with_auto_changesets = version_changes.filter((vc) => vc.will_generate_changeset);

		if (with_changesets.length > 0) {
			lines.push('## Version Changes (from changesets)');
			lines.push('');
			lines.push('| Package | From | To | Bump | Major |');
			lines.push('|---------|------|----|------|-------|');
			for (const change of with_changesets) {
				const is_major = change.bump_type === 'major' ? '💥 Yes' : 'No';
				lines.push(
					`| \`${change.package_name}\` | ${change.from} | ${change.to} | ${change.bump_type} | ${is_major} |`,
				);
			}
			lines.push('');
		}

		if (with_escalation.length > 0) {
			lines.push('## Version Changes (bump escalation required)');
			lines.push('');
			lines.push('| Package | From | To | Changesets Bump | Required Bump | Major |');
			lines.push('|---------|------|-----|-----------------|---------------|-------|');
			for (const change of with_escalation) {
				const is_major = change.bump_type === 'major' ? '💥 Yes' : 'No';
				lines.push(
					`| \`${change.package_name}\` | ${change.from} | ${change.to} | ${change.existing_bump} | ${change.required_bump} | ${is_major} |`,
				);
			}
			lines.push('');
			lines.push(
				'> ⬆️ These packages have changesets, but dependencies require a larger version bump.',
			);
			lines.push('');
		}

		if (with_auto_changesets.length > 0) {
			lines.push('## Version Changes (auto-generated for dependency updates)');
			lines.push('');
			lines.push('| Package | From | To | Bump | Major |');
			lines.push('|---------|------|-----|------|-------|');
			for (const change of with_auto_changesets) {
				const is_major = change.bump_type === 'major' ? '💥 Yes' : 'No';
				lines.push(
					`| \`${change.package_name}\` | ${change.from} | ${change.to} | ${change.bump_type} | ${is_major} |`,
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
					`- "${update.updated_dependency}": "${update.current_version}"  # ${type_label}`,
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

	// Info (packages with no changes - normal status)
	if (info.length > 0) {
		lines.push('## ℹ️ No Changes to Publish');
		lines.push('');
		lines.push('*These packages have no changesets and no dependency updates:*');
		lines.push('');
		for (const pkg of info) {
			lines.push(`- \`${pkg}\``);
		}
		lines.push('');
	}

	// Summary
	const major_bump_count = version_changes.filter((vc) => vc.bump_type === 'major').length;
	lines.push('## Summary');
	lines.push('');
	lines.push(`- **Packages to publish**: ${version_changes.length}`);
	lines.push(`- **Dependency updates**: ${dependency_updates.length}`);
	lines.push(`- **Major version bumps**: ${major_bump_count}`);
	lines.push(`- **Warnings**: ${warnings.length}`);
	lines.push(`- **Errors**: ${errors.length}`);

	return lines;
};
