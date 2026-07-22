/**
 * Side-effect preview for a publishing plan.
 *
 * `derive_publish_steps` linearizes a frozen `PublishingPlan` into the ordered side-effects
 * a `--wetrun` would perform, mirroring `execute_publishing_plan`'s pass so the preview and
 * the executor read the same plan data and can't drift. Pure — no side effects.
 *
 * @module
 */

import type { BumpType } from './version_utils.ts';
import type { PublishingPlan, VersionChange } from './publishing_plan.ts';
import { UnreachableError } from '@fuzdev/fuz_util/error.ts';

/** How a package's version bump arises in the plan. */
export type PublishStepVia = 'changeset' | 'auto_changeset' | 'escalation';

/** One ordered side-effect a wetrun would perform. */
export type PublishStep =
	| { kind: 'publish'; repo: string; from: string; to: string; bump: BumpType; via: PublishStepVia }
	| { kind: 'npm_wait'; repo: string; version: string }
	| {
			kind: 'dependency_update';
			dependent: string;
			dependency: string;
			to: string;
			dep_type: 'prod' | 'peer';
			creates_changeset: boolean;
	  }
	| { kind: 'dev_dep_update'; repo: string; dependency: string; to: string }
	| { kind: 'deploy'; repo: string; builds: boolean };

export interface DerivePublishStepsOptions {
	/** Include the deploy phase (the publisher only deploys with `--deploy`). */
	deploy?: boolean;
}

const step_via = (change: VersionChange): PublishStepVia =>
	change.needs_bump_escalation
		? 'escalation'
		: change.has_changesets
			? 'changeset'
			: 'auto_changeset';

/**
 * Derives the ordered side-effects a wetrun would perform from a frozen plan.
 *
 * Reads only `publishing_order`, `version_changes`, and `dependency_updates` — the same data
 * `execute_publishing_plan` consumes, in the same order — so the preview reflects the real
 * pass. A dependency is only propagated to its dependents if it actually publishes this run.
 */
export const derive_publish_steps = (
	plan: PublishingPlan,
	options: DerivePublishStepsOptions = {}
): Array<PublishStep> => {
	const { deploy = false } = options;
	const changes: Map<string, VersionChange> = new Map(
		plan.version_changes.map((vc) => [vc.package_name, vc])
	);

	const steps: Array<PublishStep> = [];
	const changed: Set<string> = new Set(); // for the deploy phase: published + their dependents

	// Phase 1: one pass over the topological order, mirroring `execute_publishing_plan`.
	for (const repo of plan.publishing_order) {
		const change = changes.get(repo);
		if (!change) continue; // not in the plan = nothing to publish

		steps.push({
			kind: 'publish',
			repo,
			from: change.from,
			to: change.to,
			bump: change.bump_type,
			via: step_via(change)
		});
		steps.push({ kind: 'npm_wait', repo, version: change.to });
		changed.add(repo);

		// Prod/peer dependents are rewritten right after this publishes. A dependent that
		// republishes (has its own plan version change) gets an auto-changeset and republishes in
		// turn (its rewritten deps are installed + healed by its own `gro publish`); a private
		// dependent has no version change, so it's an update-only leaf — range rewritten, no
		// changeset.
		for (const update of plan.dependency_updates) {
			if (update.updated_dependency !== repo) continue;
			if (update.type !== 'dependencies' && update.type !== 'peerDependencies') continue;
			const republishes = changes.has(update.dependent_package);
			steps.push({
				kind: 'dependency_update',
				dependent: update.dependent_package,
				dependency: repo,
				to: change.to,
				dep_type: update.type === 'peerDependencies' ? 'peer' : 'prod',
				creates_changeset: republishes
			});
			changed.add(update.dependent_package);
		}
	}

	// Phase 2: dev-dependency updates for deps that published this run — committed without a
	// changeset (dev-only changes redeploy, they don't republish). No install step: these repos
	// don't run `gro publish`; gro installs + heals their deps when they next build/deploy/sync.
	for (const update of plan.dependency_updates) {
		if (update.type !== 'devDependencies') continue;
		const dep_change = changes.get(update.updated_dependency);
		if (!dep_change) continue;
		steps.push({
			kind: 'dev_dep_update',
			repo: update.dependent_package,
			dependency: update.updated_dependency,
			to: dep_change.to
		});
		changed.add(update.dependent_package);
	}

	// Phase 3: deploy every changed repo (only with --deploy). Each builds fresh.
	if (deploy) {
		for (const repo of changed) {
			steps.push({ kind: 'deploy', repo, builds: true });
		}
	}

	return steps;
};

/**
 * Formats steps as human-readable lines (one per step) for stdout and markdown output.
 * Returns a single placeholder line when there are no side effects.
 */
export const format_publish_steps = (steps: Array<PublishStep>): Array<string> => {
	if (steps.length === 0) return ['(no side effects — nothing to publish)'];
	return steps.map((step) => {
		switch (step.kind) {
			case 'publish':
				return `publish   ${step.repo}  ${step.from} → ${step.to}  (${step.bump}, ${step.via})`;
			case 'npm_wait':
				return `npm wait  ${step.repo}@${step.version}`;
			case 'dependency_update':
				return `update    ${step.dependent} ← ${step.dependency}@${step.to}  (${step.dep_type}${step.creates_changeset ? ', changeset' : ''})`;
			case 'dev_dep_update':
				return `dev dep   ${step.repo} ← ${step.dependency}@${step.to}  (no changeset)`;
			case 'deploy':
				return `deploy    ${step.repo}${step.builds ? '  (builds)' : ''}`;
			default:
				throw new UnreachableError(step);
		}
	});
};
