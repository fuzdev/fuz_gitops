/**
 * Structured events for multi-repo publishing.
 *
 * Publishing emits a stream of tagged events alongside its human-readable logging,
 * so machine consumers (CI, dashboards) can follow a run step by step. Every run
 * opens with a `run_started` event carrying `wetrun`: when `false`, the run is a dry
 * run and every `package_completed` is a prediction (its `commit` is `'simulated'`)
 * rather than an applied change. A run's `run_finished` summary is derived from the
 * same event list via `summarize_events`, so the stream and the summary never drift.
 *
 * Events are consumed through a `PublishingEventHandler` sink (see
 * `publishing_event_handler.ts`).
 *
 * @module
 */

import { z } from 'zod';

/**
 * Coarse triage classification for a failed package. Lets consumers branch on
 * failure kind without parsing the message.
 */
export const PublishingErrorCode = z.enum([
	'publish',
	'network',
	'auth',
	'dependency',
	'build',
	// the real published version diverged from the frozen plan's prediction — an
	// invariant violation, distinct from an ordinary publish failure (see fail-loud
	// drift detection in `multi_repo_publisher.ts`)
	'drift',
	'other'
]);
export type PublishingErrorCode = z.infer<typeof PublishingErrorCode>;

/** Tallied outcome of a publishing run, derived from its events via `summarize_events`. */
export const PublishingRunSummary = z.strictObject({
	total: z.number().meta({ description: 'packages in the publishing order (the candidate set)' }),
	published: z.number().meta({ description: 'packages published (or, in a dry run, predicted)' }),
	failed: z.number(),
	skipped: z.number(),
	duration: z.number().meta({ description: 'wall-clock duration in milliseconds' })
});
export type PublishingRunSummary = z.infer<typeof PublishingRunSummary>;

/**
 * A single structured event emitted during a publishing run. Tagged on `event` so the
 * union serializes as one self-describing JSON object per event (JSON-lines on the wire).
 */
export const PublishingEvent = z.discriminatedUnion('event', [
	z.strictObject({
		event: z.literal('run_started'),
		wetrun: z
			.boolean()
			.meta({ description: 'false means every package_completed is a prediction' }),
		total: z.number()
	}),
	z.strictObject({
		event: z.literal('package_skipped'),
		name: z.string(),
		reason: z.string()
	}),
	z.strictObject({
		event: z.literal('package_completed'),
		name: z.string(),
		old_version: z.string(),
		new_version: z.string(),
		// mirrors `BumpType` from `version_utils.ts`; inline so the event schema is self-contained
		bump_type: z.enum(['major', 'minor', 'patch']),
		breaking: z.boolean(),
		commit: z.string().meta({ description: "'simulated' in a dry run, otherwise the commit hash" }),
		tag: z.string()
	}),
	z.strictObject({
		event: z.literal('npm_waited'),
		name: z.string(),
		version: z.string().meta({ description: 'the version waited on after publishing' })
	}),
	z.strictObject({
		event: z.literal('package_failed'),
		name: z.string(),
		error: z.string(),
		code: PublishingErrorCode
	}),
	z.strictObject({
		event: z.literal('dependency_updated'),
		dependent: z.string(),
		dependency: z.string(),
		version: z.string(),
		// 'prod'/'peer' updates run inline in the publish pass; 'dev' updates run in the later
		// dev-dependency pass. Lets a consumer reconstruct which side-effect phase this is.
		dep_type: z.enum(['prod', 'peer', 'dev']),
		// true when the update creates an auto-changeset (a publishable dependent republishes);
		// false for dev-dep updates and update-only leaves (private dependents never publish).
		creates_changeset: z.boolean()
	}),
	z.strictObject({
		event: z.literal('deploy_started'),
		name: z.string()
	}),
	z.strictObject({
		event: z.literal('deploy_completed'),
		name: z.string()
	}),
	z.strictObject({
		event: z.literal('deploy_failed'),
		name: z.string(),
		error: z.string()
	}),
	z.strictObject({
		event: z.literal('run_finished'),
		summary: PublishingRunSummary
	})
]);
export type PublishingEvent = z.infer<typeof PublishingEvent>;

/**
 * Derives a run summary from the captured event list — the single canonical path from
 * events to summary, so the `run_finished` summary always agrees with the stream.
 * Call before emitting `run_finished` (which is not itself counted).
 *
 * @param events - the events captured so far this run
 * @param duration - wall-clock duration in milliseconds
 */
export const summarize_events = (
	events: Array<PublishingEvent>,
	duration: number
): PublishingRunSummary => {
	let total = 0;
	let published = 0;
	let failed = 0;
	let skipped = 0;
	for (const event of events) {
		switch (event.event) {
			case 'run_started':
				total = event.total;
				break;
			case 'package_completed':
				published++;
				break;
			case 'package_failed':
				failed++;
				break;
			case 'package_skipped':
				skipped++;
				break;
			default:
				break;
		}
	}
	return { total, published, failed, skipped, duration };
};
