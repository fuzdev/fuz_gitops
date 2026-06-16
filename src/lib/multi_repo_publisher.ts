import type {Logger} from '@fuzdev/fuz_util/log.ts';
import {TaskError} from '@fuzdev/gro';
import {join} from 'node:path';
import {styleText as st} from 'node:util';

import {repo_is_npm, type LocalRepo} from './local_repo.ts';
import {update_package_json, type VersionStrategy} from './dependency_updater.ts';
import {
	generate_publishing_plan,
	type VersionChange,
	type DependencyUpdate,
	type PublishingPlan,
} from './publishing_plan.ts';
import type {PreflightOptions} from './preflight_checks.ts';
import type {GitopsOperations} from './operations.ts';
import {default_gitops_operations} from './operations_defaults.ts';
import {GITOPS_NPM_WAIT_TIMEOUT_DEFAULT} from './gitops_constants.ts';
import {
	type PublishingEvent,
	type PublishingRunSummary,
	summarize_events,
} from './publishing_event.ts';
import {
	type PublishingEventHandler,
	capture_handler,
	multi_handler,
} from './publishing_event_handler.ts';

export interface PublishingOptions {
	wetrun: boolean;
	version_strategy?: VersionStrategy;
	deploy?: boolean;
	max_wait?: number;
	log?: Logger;
	ops?: GitopsOperations;
	/** Structured event sink; defaults to capture-only (events surface on the result). */
	events?: PublishingEventHandler;
}

export interface PublishedVersion {
	name: string;
	old_version: string;
	new_version: string;
	bump_type: 'major' | 'minor' | 'patch';
	breaking: boolean;
	commit: string;
	tag: string;
}

export interface PublishingResult {
	ok: boolean;
	published: Array<PublishedVersion>;
	failed: Array<{name: string; error: Error}>;
	duration: number;
	/** The structured event stream for this run, in emission order. */
	events: Array<PublishingEvent>;
	/** Tallied outcome, derived from `events`. */
	summary: PublishingRunSummary;
	/** Plan errors that blocked (wetrun) or would block (dry run) publishing; empty when clean. */
	plan_errors: Array<string>;
	/** Non-blocking plan warnings (e.g. the fixed-point iteration limit). */
	plan_warnings: Array<string>;
}

export const publish_repos = async (
	repos: Array<LocalRepo>,
	options: PublishingOptions,
): Promise<PublishingResult> => {
	const {log, ops = default_gitops_operations} = options;
	// Convenience entry: generate the plan, then execute it. Callers that already hold a
	// plan (e.g. to display and confirm it first) call `execute_publishing_plan` directly,
	// so the plan is generated exactly once per command.
	const plan = await generate_publishing_plan(repos, {log, ops: ops.changeset});
	return execute_publishing_plan(repos, plan, options);
};

/**
 * Executes a frozen publishing plan in a single linear pass — the "dumb executor" half of
 * the zap model. The plan is the single source of truth; this re-derives nothing.
 *
 * Fails loud when the plan couldn't be fully computed (`plan.errors`): a wetrun aborts
 * before any side effect, a dry run still reports the partial cascade but returns
 * `ok: false`. This is the single error gate — `--no-plan` can't bypass it.
 */
export const execute_publishing_plan = async (
	all_repos: Array<LocalRepo>,
	plan: PublishingPlan,
	options: PublishingOptions,
): Promise<PublishingResult> => {
	const start_time = Date.now();
	const {wetrun, log, ops = default_gitops_operations} = options;

	// Only npm repos publish; drop any non-npm repos (e.g. cargo) so preflight and the
	// executor's name lookup never touch them. The plan already excludes them too.
	const repos = all_repos.filter(repo_is_npm);

	// Fail loud on an incomplete plan. Executing one with errors would silently skip the
	// affected packages. A wetrun aborts before touching npm or git; a dry run proceeds
	// (no side effects) but the result is not ok (see the summary below).
	if (wetrun && plan.errors.length > 0) {
		throw new TaskError(
			`Cannot publish — the plan has ${plan.errors.length} error(s):\n  ${plan.errors.join('\n  ')}`,
		);
	}

	// Capture every event for the result; also forward to the caller's sink if provided.
	const capture = capture_handler();
	const events_handler = options.events ? multi_handler([options.events, capture]) : capture;
	const emit = (event: PublishingEvent): void => {
		events_handler.emit(event);
	};

	// Preflight checks (skip for dry runs since we're not actually publishing)
	if (wetrun) {
		const preflight_options: PreflightOptions = {
			skip_changesets: false, // Always check for changesets
			required_branch: 'main',
			log,
		};
		const preflight = await ops.preflight.run_preflight_checks({
			repos,
			preflight_options,
			git_ops: ops.git,
			npm_ops: ops.npm,
			build_ops: ops.build,
			changeset_ops: ops.changeset,
		});

		if (!preflight.ok) {
			throw new TaskError(`Preflight checks failed: ${preflight.errors.join(', ')}`);
		}
	} else {
		log?.info('⏭️  Skipping preflight checks (dry run)');
	}

	// The plan already validated the dependency graph and resolved the full cascade
	// (explicit changesets, bump escalations, and auto-generated changesets) — execute its
	// frozen topological order directly, re-deriving nothing.
	const order = plan.publishing_order;
	const plan_changes: Map<string, VersionChange> = new Map(
		plan.version_changes.map((vc) => [vc.package_name, vc]),
	);
	if (order.length > 0) {
		log?.info(`  Publishing order: ${order.join(' → ')}`);
	}

	emit({event: 'run_started', wetrun, total: order.length});

	const published: Map<string, PublishedVersion> = new Map();
	const failed: Map<string, Error> = new Map();
	const changed_repos: Set<string> = new Set(); // Track repos with any changes for selective deployment
	// Name → repo index, built once to avoid repeated linear scans of `repos`.
	const repo_by_name: Map<string, LocalRepo> = new Map(repos.map((r) => [r.library.name, r]));

	log?.info(st('cyan', `\n🚀 ${wetrun ? 'Publishing' : 'Dry run'}...\n`));

	// Phase 1: one linear pass over the plan's topological order. The plan already
	// resolved the full cascade, and publishing a package immediately rewrites every
	// dependent's package.json and creates its auto-changeset — so by the time the pass
	// reaches any package, all its dependencies have published and its changeset exists.
	// No fixed-point loop is needed: a single pass converges by construction.
	for (let i = 0; i < order.length; i++) {
		const pkg_name = order[i]!;
		const planned = plan_changes.get(pkg_name);

		// Not in the plan = no changesets and no dependency updates = nothing to publish.
		if (!planned) {
			emit({event: 'package_skipped', name: pkg_name, reason: 'no changesets'});
			log?.info(st('yellow', `  ⚠️  Skipping ${pkg_name} - no changesets`));
			continue;
		}

		const repo = repo_by_name.get(pkg_name);
		if (!repo) continue;

		// An earlier publish may have rewritten this package's dependency ranges. No install is
		// needed here: `gro publish` runs its own install (which self-heals npm's stale-cache
		// ETARGET), so the package's freshly-rewritten deps are installed and healed as part of
		// publishing it.
		try {
			// 1. Publish this package (real publish or dry-run prediction)
			log?.info(
				st(
					'dim',
					`  [${i + 1}/${order.length}] ${wetrun ? 'Publishing' : 'Would publish'} ${pkg_name}...`,
				),
			);
			const version = await publish_single_repo(repo, options, ops, planned);

			// Fail loud on drift: a real publish that lands a version the plan didn't predict
			// is an invariant violation, not a routine failure. Abort and leave the dirty
			// state in place — re-running re-plans from the current state (the just-published
			// package no longer has changesets, so it drops out of the new plan).
			if (wetrun && version.new_version !== planned.to) {
				const err = new Error(
					`Plan drift for ${pkg_name}: published ${version.new_version} but the plan predicted ${planned.to}. ` +
						`Aborting — re-run 'gro gitops_publish --wetrun' to re-plan from the current state.`,
				);
				failed.set(pkg_name, err);
				emit({event: 'package_failed', name: pkg_name, error: err.message, code: 'drift'});
				log?.error(st('red', `  ❌ ${err.message}`));
				break;
			}

			published.set(pkg_name, version);
			changed_repos.add(pkg_name); // Mark as changed for deployment
			emit({
				event: 'package_completed',
				name: pkg_name,
				old_version: version.old_version,
				new_version: version.new_version,
				bump_type: version.bump_type,
				breaking: version.breaking,
				commit: version.commit,
				tag: version.tag,
			});
			log?.info(
				wetrun
					? st('green', `  ✅ Published ${pkg_name}@${version.new_version}`)
					: st('cyan', `  ◇ Would publish ${pkg_name}@${version.new_version}`),
			);

			if (wetrun) {
				// 2. Wait for this package to be available on NPM
				log?.info(`  ⏳ Waiting for ${pkg_name}@${version.new_version} on NPM...`);
				const wait_result = await ops.npm.wait_for_package({
					pkg: pkg_name,
					version: version.new_version,
					wait_options: {
						max_attempts: 30,
						initial_delay: 1000,
						max_delay: 60000,
						timeout: options.max_wait ?? GITOPS_NPM_WAIT_TIMEOUT_DEFAULT,
					},
					log,
				});

				if (!wait_result.ok) {
					// Handle inline (don't throw into the generic catch): the npm-wait failure
					// carries a typed `timeout` signal, so we know this is a network failure
					// without sniffing the message.
					const err = new Error(
						`Failed to wait for package: ${wait_result.message}${wait_result.timeout ? ' (timeout)' : ''}`,
					);
					failed.set(pkg_name, err);
					emit({event: 'package_failed', name: pkg_name, error: err.message, code: 'network'});
					log?.error(st('red', `  ❌ Failed to publish ${pkg_name}: ${err.message}`));
					break; // fail fast
				}
				emit({event: 'npm_waited', name: pkg_name, version: version.new_version});

				// 3. Update every dependent the plan says has a prod/peer dep on this package.
				// This rewrites their package.json ranges and creates their auto-changeset,
				// which a later step of this same pass publishes. The dependent is queued for
				// a single install just before it publishes (see the top of the loop).
				const dependent_updates = group_dependency_updates(
					plan.dependency_updates,
					published,
					(update) =>
						update.updated_dependency === pkg_name &&
						(update.type === 'dependencies' || update.type === 'peerDependencies'),
				);
				for (const [dependent_name, updates] of dependent_updates) {
					const dependent_repo = repo_by_name.get(dependent_name);
					if (!dependent_repo) continue;
					// A dependent republishes iff the plan gave it a version change. Private packages
					// are excluded from the plan's version changes, so they take the update-only-leaf
					// path: rewrite + commit their dependency ranges with NO changeset and NO
					// publish/npm-wait. Publishable dependents get an auto-changeset and republish in
					// turn — `gro publish` installs + heals their rewritten deps when it reaches them.
					const republishes = plan_changes.has(dependent_name);
					for (const [dep_name, dep_version] of updates) {
						log?.info(`    Updating ${dependent_name}'s dependency on ${dep_name}`);
						emit({
							event: 'dependency_updated',
							dependent: dependent_name,
							dependency: dep_name,
							version: dep_version,
							dep_type: dependency_update_type(plan, dependent_name, dep_name),
							creates_changeset: republishes,
						});
					}
					changed_repos.add(dependent_name); // Mark as changed for deployment
					if (republishes) {
						await update_package_json(dependent_repo, updates, {
							strategy: options.version_strategy || 'caret',
							published_versions: published, // creates the auto-changeset
							log,
							git_ops: ops.git,
							fs_ops: ops.fs,
						});
					} else {
						// update-only leaf: rewrite ranges + commit, no changeset (it won't republish)
						await update_package_json(dependent_repo, updates, {
							strategy: options.version_strategy || 'caret',
							log,
							git_ops: ops.git,
							fs_ops: ops.fs,
						});
					}
				}
			}
		} catch (error) {
			const err = error instanceof Error ? error : new Error(String(error));
			failed.set(pkg_name, err);
			emit({
				event: 'package_failed',
				name: pkg_name,
				error: err.message,
				// TODO: emit a precise code once the npm/process ops return typed errors —
				// today a publish-step cause lives in unstructured stderr, so use the honest
				// coarse bucket rather than guessing 'auth'/'network'/'build' from the message.
				code: 'publish',
			});
			log?.error(st('red', `  ❌ Failed to publish ${pkg_name}: ${err.message}`));
			break; // Always fail fast on error
		}
	}

	// Phase 2: Update all dev dependencies (can have cycles)
	// Dev dep changes require deployment even without version bumps (rebuild needed).
	// Sourced from the plan's dependency updates so the publisher derives nothing itself.
	// Only package.json is rewritten + committed here — no install. These repos don't run
	// `gro publish`; their `node_modules` is refreshed (and ETARGET-healed) by gro the next
	// time they build/deploy/sync, so the executor never runs a bare `npm install` itself.
	if (published.size > 0 && wetrun) {
		const dev_updates_by_repo = group_dependency_updates(
			plan.dependency_updates,
			published,
			(update) => update.type === 'devDependencies',
		);

		if (dev_updates_by_repo.size > 0) {
			log?.info(st('cyan', '\n🔄 Updating dev dependencies...\n'));
		}

		for (const [repo_name, dev_updates] of dev_updates_by_repo) {
			const repo = repo_by_name.get(repo_name);
			if (!repo) continue;

			log?.info(`  Updating ${dev_updates.size} dev dependencies in ${repo_name}`);
			for (const [dep_name, dep_version] of dev_updates) {
				emit({
					event: 'dependency_updated',
					dependent: repo_name,
					dependency: dep_name,
					version: dep_version,
					dep_type: 'dev',
					creates_changeset: false, // dev-dep updates redeploy without republishing
				});
			}
			changed_repos.add(repo_name); // Mark as changed for deployment
			// No `published_versions` here on purpose: a dev-dep bump updates and commits
			// package.json but must NOT generate a changeset — dev-only changes redeploy
			// (rebuild) without republishing, so they shouldn't bump the next release.
			await update_package_json(repo, dev_updates, {
				strategy: options.version_strategy || 'caret',
				log,
				git_ops: ops.git,
				fs_ops: ops.fs,
			});
		}
	}

	// Phase 3: Deploy repos with changes (optional)
	// Deploys only repos that were: published, had prod/peer deps updated, or had dev deps updated.
	// Iterate `changed_repos` (insertion order) rather than `repos` order so deploys run in
	// dependency order — and so the `--preview` side-effect list matches this exactly.
	if (options.deploy && wetrun) {
		const repos_to_deploy = Array.from(changed_repos)
			.map((name) => repo_by_name.get(name))
			.filter((r): r is LocalRepo => r !== undefined);
		log?.info(
			st(
				'cyan',
				`\n🚢 Deploying ${repos_to_deploy.length}/${repos.length} repos with changes...\n`,
			),
		);

		for (const repo of repos_to_deploy) {
			try {
				emit({event: 'deploy_started', name: repo.library.name});
				log?.info(`  Deploying ${repo.library.name}...`);
				// Build fresh (no --no-build): a deployed site bundles its dependencies, so it
				// must be rebuilt against the versions this run just published — the preflight
				// build ran against the old versions, before the cascade rewrote package.json.
				const deploy_result = await ops.process.spawn({
					cmd: 'gro',
					args: ['deploy'],
					cwd: repo.repo_dir,
				});

				if (deploy_result.ok) {
					emit({event: 'deploy_completed', name: repo.library.name});
					log?.info(st('green', `  ✅ Deployed ${repo.library.name}`));
				} else {
					emit({event: 'deploy_failed', name: repo.library.name, error: deploy_result.message});
					log?.warn(st('yellow', `  ⚠️  Failed to deploy ${repo.library.name}`));
				}
			} catch (error) {
				const err = error instanceof Error ? error : new Error(String(error));
				emit({event: 'deploy_failed', name: repo.library.name, error: err.message});
				log?.error(st('red', `  ❌ Error deploying ${repo.library.name}: ${err.message}`));
			}
		}
	}

	// Summary
	const duration = Date.now() - start_time;
	const ok = failed.size === 0 && plan.errors.length === 0;
	const summary = summarize_events(capture.events, duration);

	log?.info(st('cyan', `\n📋 ${wetrun ? 'Publishing' : 'Dry Run'} Summary\n`));
	log?.info(`  Duration: ${(duration / 1000).toFixed(1)}s`);
	log?.info(`  ${wetrun ? 'Published' : 'Would publish'}: ${published.size} packages`);
	if (failed.size > 0) {
		log?.info(`  Failed: ${failed.size} packages`);
	}

	// Surface plan diagnostics (a wetrun with errors threw above; this is mainly the dry run
	// and non-blocking warnings) so an audit of the cascade sees them.
	if (plan.warnings.length > 0) {
		log?.warn(st('yellow', `  ⚠️  Plan warnings: ${plan.warnings.length}`));
		for (const warning of plan.warnings) log?.warn(st('yellow', `     - ${warning}`));
	}
	if (plan.errors.length > 0) {
		log?.error(st('red', `  ❌ Plan errors: ${plan.errors.length}`));
		for (const plan_error of plan.errors) log?.error(st('red', `     - ${plan_error}`));
	}

	if (ok) {
		if (wetrun) {
			log?.info(st('green', '\n✨ All packages published successfully!\n'));
		} else {
			log?.info(
				st(
					'green',
					`\n✨ Dry run complete — ${published.size} package(s) would be published. Re-run with --wetrun to publish.`,
				),
			);
			// The dry run is driven by the same plan as `gro gitops_plan`, so this
			// count includes bump escalations and auto-generated changesets — it
			// matches `gro gitops_plan` exactly (the full cascade).
			log?.info(st('dim', 'This matches `gro gitops_plan` (the full cascade).\n'));
		}
	} else {
		log?.error(
			st(
				'red',
				wetrun
					? '\n❌ Some packages failed to publish\n'
					: '\n❌ Some packages failed during dry run\n',
			),
		);
	}

	emit({event: 'run_finished', summary});

	return {
		ok,
		published: Array.from(published.values()),
		failed: Array.from(failed.entries()).map(([name, error]) => ({name, error})),
		duration,
		events: capture.events,
		summary,
		plan_errors: plan.errors,
		plan_warnings: plan.warnings,
	};
};

/**
 * Publishes a single repo using `gro publish`.
 *
 * Dry run mode: reports the precomputed plan entry without side effects.
 * Real mode: runs `gro publish --no-build` (builds already validated in preflight),
 * reads the new version from `package.json`, and returns it alongside the plan's
 * predicted bump metadata. The caller compares the read-back version to the plan to
 * detect drift.
 *
 * @throws {Error} if the publish, version read-back, or commit-hash lookup fails
 */
const publish_single_repo = async (
	repo: LocalRepo,
	options: PublishingOptions,
	ops: GitopsOperations,
	planned: VersionChange,
): Promise<PublishedVersion> => {
	const {wetrun} = options;

	if (!wetrun) {
		// Dry run reports the precomputed plan — the single source of truth for the
		// cascade (explicit changesets, bump escalations, and auto-generated changesets).
		return {
			name: repo.library.name,
			old_version: planned.from,
			new_version: planned.to,
			bump_type: planned.bump_type,
			breaking: planned.breaking,
			commit: 'simulated',
			tag: `v${planned.to}`,
		};
	}

	// Run gro publish with --no-build (builds were validated in preflight checks)
	const publish_result = await ops.process.spawn({
		cmd: 'gro',
		args: ['publish', '--no-build'],
		cwd: repo.repo_dir,
	});

	if (!publish_result.ok) {
		throw new Error(`Failed to publish ${repo.library.name}: ${publish_result.message}`);
	}

	// Read the new version from package.json after gro publish
	const package_json_path = join(repo.repo_dir, 'package.json');
	const content_result = await ops.fs.readFile({path: package_json_path, encoding: 'utf8'});

	if (!content_result.ok) {
		throw new Error(`Failed to read package.json: ${content_result.message}`);
	}

	const package_json = JSON.parse(content_result.value);
	const new_version = package_json.version;

	// Get actual commit hash
	const commit_result = await ops.git.current_commit_hash({cwd: repo.repo_dir});

	if (!commit_result.ok) {
		throw new Error(`Failed to get commit hash: ${commit_result.message}`);
	}

	const commit = commit_result.value;

	// Bump metadata comes from the plan (the single source of truth); the caller
	// fail-louds if `new_version` diverges from the plan's prediction.
	return {
		name: repo.library.name,
		old_version: planned.from,
		new_version,
		bump_type: planned.bump_type,
		breaking: planned.breaking,
		commit,
		tag: `v${new_version}`,
	};
};

/**
 * Groups dependency updates by dependent package — `dependent → (dependency → new
 * version)`, the shape `update_package_json` consumes. Restricted by `predicate` (e.g.
 * prod/peer for a given package, or all dev deps) and to dependencies that actually
 * published this run, so a failed/aborted publish never propagates to its dependents.
 */
export const group_dependency_updates = (
	updates: Array<DependencyUpdate>,
	published: Map<string, PublishedVersion>,
	predicate: (update: DependencyUpdate) => boolean,
): Map<string, Map<string, string>> => {
	const by_repo: Map<string, Map<string, string>> = new Map();
	for (const update of updates) {
		if (!predicate(update)) continue;
		const published_dep = published.get(update.updated_dependency);
		if (!published_dep) continue;
		let repo_updates = by_repo.get(update.dependent_package);
		if (!repo_updates) {
			repo_updates = new Map();
			by_repo.set(update.dependent_package, repo_updates);
		}
		repo_updates.set(update.updated_dependency, published_dep.new_version);
	}
	return by_repo;
};

/** The dep_type tag for a prod/peer dependency-update event — `peer` if the edge is a peer
 * dependency, else `prod`. Mirrors `derive_publish_steps`' classification so the executor's
 * event stream and the preview agree. */
const dependency_update_type = (
	plan: PublishingPlan,
	dependent: string,
	dependency: string,
): 'prod' | 'peer' =>
	plan.dependency_updates.some(
		(u) =>
			u.dependent_package === dependent &&
			u.updated_dependency === dependency &&
			u.type === 'peerDependencies',
	)
		? 'peer'
		: 'prod';
