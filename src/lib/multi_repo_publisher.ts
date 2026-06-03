import type {Logger} from '@fuzdev/fuz_util/log.js';
import {TaskError} from '@fuzdev/gro';
import {join} from 'node:path';
import {styleText as st} from 'node:util';

import type {LocalRepo} from './local_repo.js';
import {update_package_json, type VersionStrategy} from './dependency_updater.js';
import {validate_dependency_graph} from './graph_validation.js';
import type {PreflightOptions} from './preflight_checks.js';
import {needs_update, is_breaking_change, detect_bump_type} from './version_utils.js';
import type {GitopsOperations} from './operations.js';
import {default_gitops_operations} from './operations_defaults.js';
import {
	GITOPS_MAX_ITERATIONS_DEFAULT,
	GITOPS_NPM_WAIT_TIMEOUT_DEFAULT,
} from './gitops_constants.js';
import {install_with_cache_healing} from './npm_install_helpers.js';
import {
	type PublishingEvent,
	type PublishingRunSummary,
	summarize_events,
} from './publishing_event.js';
import {
	type PublishingEventHandler,
	capture_handler,
	multi_handler,
} from './publishing_event_handler.js';

export interface PublishingOptions {
	wetrun: boolean;
	update_deps: boolean;
	version_strategy?: VersionStrategy;
	deploy?: boolean;
	max_wait?: number;
	skip_install?: boolean;
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
}

export const publish_repos = async (
	repos: Array<LocalRepo>,
	options: PublishingOptions,
): Promise<PublishingResult> => {
	const start_time = Date.now();
	const {wetrun, update_deps, log, ops = default_gitops_operations} = options;

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

	// Build dependency graph and validate
	const {publishing_order: order} = validate_dependency_graph(repos, {
		log,
		throw_on_prod_cycles: true,
		log_cycles: true,
		log_order: true,
	});

	emit({event: 'run_started', wetrun, total: order.length});

	const published: Map<string, PublishedVersion> = new Map();
	const failed: Map<string, Error> = new Map();
	const changed_repos: Set<string> = new Set(); // Track repos with any changes for selective deployment
	const skipped_packages: Set<string> = new Set(); // dedupe the package_skipped event across iterations

	// Fixed-point iteration: keep publishing until no new changesets are created
	// This handles transitive dependency updates (auto-generated changesets)
	let iteration = 0;
	let converged = false;

	while (!converged && iteration < GITOPS_MAX_ITERATIONS_DEFAULT) {
		iteration++;
		emit({event: 'iteration_started', iteration, max: GITOPS_MAX_ITERATIONS_DEFAULT});
		log?.info(
			st(
				'cyan',
				`\n🚀 ${wetrun ? 'Publishing' : 'Dry run'} iteration ${iteration}/${GITOPS_MAX_ITERATIONS_DEFAULT}...\n`,
			),
		);

		// Track if any packages were published in this iteration
		let published_in_iteration = false;
		let published_count = 0;

		// Track repos changed in THIS iteration only (for batch install)
		const changed_in_iteration: Set<string> = new Set();

		// Phase 1: Publish each package and immediately update dependents
		for (let i = 0; i < order.length; i++) {
			const pkg_name = order[i]!;
			const repo = repos.find((r) => r.library.name === pkg_name);
			if (!repo) continue;

			// Skip if already published in a previous iteration
			if (published.has(pkg_name)) {
				continue;
			}

			// Check for changesets (both dry and real runs)
			const has_result = await ops.changeset.has_changesets({repo});
			if (!has_result.ok) {
				// Failed to check changesets
				const err = new Error(`Failed to check changesets: ${has_result.message}`);
				failed.set(pkg_name, err);
				emit({event: 'package_failed', name: pkg_name, error: err.message, code: 'dependency'});
				log?.error(st('red', `  ❌ ${err.message}`));
				break;
			}

			if (!has_result.value) {
				// Skip packages without changesets
				// In real publish: They might get auto-changesets during dependency updates
				// In dry run: We can't simulate auto-changesets, so just skip
				// Emit once per package — the loop revisits no-changeset packages each iteration
				if (!skipped_packages.has(pkg_name)) {
					skipped_packages.add(pkg_name);
					emit({event: 'package_skipped', name: pkg_name, reason: 'no changesets'});
				}
				if (!wetrun) {
					// Silent skip in dry run - plan shows which packages get auto-changesets
					continue;
				} else {
					log?.info(st('yellow', `  ⚠️  Skipping ${pkg_name} - no changesets`));
					continue;
				}
			}

			try {
				// 1. Publish this package
				log?.info(
					st(
						'dim',
						`  [${i + 1}/${order.length}] ${wetrun ? 'Publishing' : 'Would publish'} ${pkg_name}...`,
					),
				);
				const version = await publish_single_repo(repo, options, ops);
				published.set(pkg_name, version);
				changed_repos.add(pkg_name); // Mark as changed for deployment
				// Note: don't add to changed_in_iteration - published packages don't need install
				// (their dependencies didn't change, only their version)
				published_in_iteration = true;
				published_count++;
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

					// 3. Update all repos that have prod/peer deps on this package
					if (update_deps) {
						for (const dependent_repo of repos) {
							const updates: Map<string, string> = new Map();

							// Check prod dependencies
							if (dependent_repo.dependencies?.has(pkg_name)) {
								const current = dependent_repo.dependencies.get(pkg_name)!;
								if (needs_update(current, version.new_version)) {
									updates.set(pkg_name, version.new_version);
								}
							}

							// Check peer dependencies
							if (dependent_repo.peer_dependencies?.has(pkg_name)) {
								const current = dependent_repo.peer_dependencies.get(pkg_name)!;
								if (needs_update(current, version.new_version)) {
									updates.set(pkg_name, version.new_version);
								}
							}

							// Apply updates if any
							if (updates.size > 0) {
								log?.info(
									`    Updating ${dependent_repo.library.name}'s dependency on ${pkg_name}`,
								);
								emit({
									event: 'dependency_updated',
									dependent: dependent_repo.library.name,
									dependency: pkg_name,
									version: version.new_version,
								});
								changed_repos.add(dependent_repo.library.name); // Mark as changed for deployment
								changed_in_iteration.add(dependent_repo.library.name); // Track for batch install
								await update_package_json(dependent_repo, updates, {
									strategy: options.version_strategy || 'caret',
									published_versions: published,
									log,
									git_ops: ops.git,
								});
							}
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

		// Phase 1b: Batch install dependencies for repos with updated package.json
		// This ensures workspace stays consistent before next iteration
		if (wetrun && !options.skip_install && changed_in_iteration.size > 0) {
			log?.info(st('cyan', '\n📦 Installing dependencies for updated repos...\n'));
			for (const [name, err] of await install_repos(changed_in_iteration, repos, ops, emit, log)) {
				failed.set(name, err);
			}
		}

		// Log iteration summary
		if (published_count > 0) {
			log?.info(
				st(
					'dim',
					`\nIteration ${iteration}: ${published_count} package(s) ${wetrun ? 'published' : 'would be published'}\n`,
				),
			);
		}

		emit({
			event: 'iteration_finished',
			iteration,
			published_count,
			converged: !published_in_iteration,
		});

		// Check for convergence: no packages published in this iteration
		if (!published_in_iteration) {
			converged = true;
			log?.info(
				st(
					'green',
					wetrun
						? `\n✓ Converged after ${iteration} iteration(s) - no new changesets\n`
						: `\n✓ Dry run complete after ${iteration} iteration(s)\n`,
				),
			);
		} else if (iteration === GITOPS_MAX_ITERATIONS_DEFAULT) {
			// Count packages that still have changesets (not yet published)
			const pending_count = order.length - published.size;
			const estimated_iterations = Math.ceil(pending_count / 2); // Rough estimate

			log?.warn(
				st(
					'yellow',
					`\n⚠️  Reached maximum iterations (${GITOPS_MAX_ITERATIONS_DEFAULT}) without full convergence\n` +
						`    ${pending_count} package(s) may still have changesets to process\n` +
						`    Estimated ${estimated_iterations} more iteration(s) needed - run 'gro gitops_publish' again\n`,
				),
			);
		}
	}

	// Phase 2: Update all dev dependencies (can have cycles)
	// Dev dep changes require deployment even without version bumps (rebuild needed)
	const dev_updated_repos: Set<string> = new Set();
	if (update_deps && published.size > 0 && wetrun) {
		log?.info(st('cyan', '\n🔄 Updating dev dependencies...\n'));

		for (const repo of repos) {
			const dev_updates: Map<string, string> = new Map();

			// Check dev dependencies only
			if (repo.dev_dependencies) {
				for (const [dep_name, current_version] of repo.dev_dependencies) {
					const published_version = published.get(dep_name);
					if (published_version && needs_update(current_version, published_version.new_version)) {
						dev_updates.set(dep_name, published_version.new_version);
					}
				}
			}

			if (dev_updates.size > 0) {
				log?.info(`  Updating ${dev_updates.size} dev dependencies in ${repo.library.name}`);
				for (const [dep_name, dep_version] of dev_updates) {
					emit({
						event: 'dependency_updated',
						dependent: repo.library.name,
						dependency: dep_name,
						version: dep_version,
					});
				}
				changed_repos.add(repo.library.name); // Mark as changed for deployment
				dev_updated_repos.add(repo.library.name); // Track for batch install
				await update_package_json(repo, dev_updates, {
					strategy: options.version_strategy || 'caret',
					published_versions: published,
					log,
					git_ops: ops.git,
				});
			}
		}
	}

	// Phase 2b: Install dev dependencies for repos with dev dep updates
	if (wetrun && !options.skip_install && dev_updated_repos.size > 0) {
		log?.info(st('cyan', '\n📦 Installing dev dependencies for updated repos...\n'));
		for (const [name, err] of await install_repos(dev_updated_repos, repos, ops, emit, log)) {
			failed.set(name, err);
		}
	}

	// Phase 3: Deploy repos with changes (optional)
	// Deploys only repos that were: published, had prod/peer deps updated, or had dev deps updated
	if (options.deploy && wetrun) {
		const repos_to_deploy = repos.filter((r) => changed_repos.has(r.library.name));
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
				const deploy_result = await ops.process.spawn({
					cmd: 'gro',
					args: ['deploy', '--no-build'],
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
	const ok = failed.size === 0;
	const summary = summarize_events(capture.events, duration);

	log?.info(st('cyan', `\n📋 ${wetrun ? 'Publishing' : 'Dry Run'} Summary\n`));
	log?.info(`  Duration: ${(duration / 1000).toFixed(1)}s`);
	log?.info(`  ${wetrun ? 'Published' : 'Would publish'}: ${published.size} packages`);
	if (failed.size > 0) {
		log?.info(`  Failed: ${failed.size} packages`);
	}

	if (ok) {
		log?.info(
			st(
				'green',
				wetrun
					? '\n✨ All packages published successfully!\n'
					: `\n✨ Dry run complete — ${published.size} package(s) would be published. Re-run with --wetrun to publish.\n`,
			),
		);
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
	};
};

/**
 * Publishes a single repo using `gro publish`.
 *
 * Dry run mode: Predicts version from changesets without side effects.
 * Real mode: Runs `gro publish --no-build` (builds already validated in preflight),
 * reads new version from `package.json`, and returns metadata.
 *
 * @throws {Error} if changeset prediction fails (dry run) or publish fails (real)
 */
const publish_single_repo = async (
	repo: LocalRepo,
	options: PublishingOptions,
	ops: GitopsOperations = default_gitops_operations,
): Promise<PublishedVersion> => {
	const {wetrun, log} = options;

	const old_version = repo.library.package_json.version || '0.0.0';

	if (!wetrun) {
		// In dry run, predict version from changesets
		const prediction = await ops.changeset.predict_next_version({repo, log});

		if (!prediction) {
			// No changesets found, skip this repo
			throw new Error(`No changesets found for ${repo.library.name}`);
		}

		if (!prediction.ok) {
			// Error reading changesets
			throw new Error(`Failed to predict version: ${prediction.message}`);
		}

		const {version: new_version, bump_type} = prediction;
		const breaking = is_breaking_change(old_version, bump_type);

		return {
			name: repo.library.name,
			old_version,
			new_version,
			bump_type,
			breaking,
			commit: 'simulated',
			tag: `v${new_version}`,
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

	// Determine bump type and if it's breaking
	const bump_type = detect_bump_type(old_version, new_version);
	const breaking = is_breaking_change(old_version, bump_type);

	// Get actual commit hash
	const commit_result = await ops.git.current_commit_hash({cwd: repo.repo_dir});

	if (!commit_result.ok) {
		throw new Error(`Failed to get commit hash: ${commit_result.message}`);
	}

	const commit = commit_result.value;

	return {
		name: repo.library.name,
		old_version,
		new_version,
		bump_type,
		breaking,
		commit,
		tag: `v${new_version}`,
	};
};

/**
 * Installs dependencies for each named repo (with cache healing), emitting install
 * events and logging progress. A failed install doesn't stop the batch — failures are
 * collected and returned so the caller can fold them into the run's failures.
 */
const install_repos = async (
	names: Iterable<string>,
	repos: Array<LocalRepo>,
	ops: GitopsOperations,
	emit: (event: PublishingEvent) => void,
	log?: Logger,
): Promise<Map<string, Error>> => {
	const failures: Map<string, Error> = new Map();
	for (const name of names) {
		const repo = repos.find((r) => r.library.name === name);
		if (!repo) continue;
		try {
			emit({event: 'install_started', name});
			log?.info(`  Installing ${name}...`);
			await install_with_cache_healing(repo, ops, log);
			emit({event: 'install_completed', name});
			log?.info(st('green', `  ✅ Installed ${name}`));
		} catch (error) {
			const err = error instanceof Error ? error : new Error(String(error));
			failures.set(name, err);
			emit({event: 'install_failed', name, error: err.message});
			log?.error(st('red', `  ❌ Failed to install ${name}: ${err.message}`));
			// continue with other installs instead of breaking
		}
	}
	return failures;
};
