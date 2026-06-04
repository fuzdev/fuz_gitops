/**
 * Pure decision logic for the `gitops_publish` task's interactive gating.
 *
 * Extracted from `gitops_publish.task.ts` so the gating table is unit-testable without driving
 * readline or `process.exit`. The task keeps the IO — prompt, exit, logging — at its edge and
 * consults these for every branch.
 *
 * @module
 */

import type {PublishingResult} from './multi_repo_publisher.js';
import type {PublishingPlan} from './publishing_plan.js';

/** What the task should do with a generated plan before executing the cascade. */
export type PublishGate =
	| {action: 'blocked'; message: string}
	| {action: 'confirm'}
	| {action: 'proceed'};

export interface PublishGateOptions {
	/** A real publish (`--wetrun`); a dry run never prompts. */
	wetrun: boolean;
	/** Show the plan and confirm (`plan`); `--no-plan` skips the prompt. */
	show_plan: boolean;
	plan: Pick<PublishingPlan, 'errors'>;
}

/**
 * Decides whether a publish run must block, prompt for confirmation, or proceed without
 * prompting, from the pre-execution inputs.
 *
 * - `blocked`: a real publish whose plan has errors — fail loud before prompting. The executor
 *   enforces this too, so `--no-plan` can't bypass the gate; this branch only avoids prompting
 *   for (and printing the "this will publish" banner of) a plan that can't run.
 * - `confirm`: a real publish that shows its plan — the user must confirm interactively.
 * - `proceed`: a dry run, or a `--no-plan` real publish — no prompt.
 */
export const decide_publish_gate = (options: PublishGateOptions): PublishGate => {
	if (!options.wetrun || !options.show_plan) return {action: 'proceed'};
	if (options.plan.errors.length > 0) {
		return {action: 'blocked', message: 'Cannot proceed with publishing due to errors'};
	}
	return {action: 'confirm'};
};

/**
 * Whether a finished run should exit non-zero: an unsuccessful result, or a fatal error thrown
 * out of the executor.
 */
export const publish_run_failed = (
	result: Pick<PublishingResult, 'ok'>,
	fatal_error: Error | null,
): boolean => !result.ok || fatal_error !== null;
