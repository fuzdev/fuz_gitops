import type {Logger} from '@fuzdev/fuz_util/log.js';
import {styleText as st} from 'node:util';

import type {LocalRepo} from './local_repo.js';
import type {BumpType} from './semver.js';
import {validate_dependency_graph} from './graph_validation.js';
import {is_breaking_change, compare_bump_types, calculate_next_version} from './version_utils.js';
import type {ChangesetOperations} from './operations.js';
import {default_changeset_operations} from './operations_defaults.js';
import {GITOPS_MAX_ITERATIONS_DEFAULT} from './gitops_constants.js';
import type {DependencyGraph} from './dependency_graph.ts';
import {
	calculate_dependency_updates,
	get_required_bump_for_dependencies,
} from './publishing_plan_helpers.js';

// Re-export logging functions
export {log_publishing_plan, type LogPlanOptions} from './publishing_plan_logging.js';

export interface VersionChange {
	package_name: string;
	from: string;
	to: string;
	bump_type: BumpType;
	breaking: boolean;
	has_changesets: boolean;
	will_generate_changeset?: boolean; // True if changeset will be auto-generated for dependency updates
	needs_bump_escalation?: boolean; // True if existing changesets need escalation for dependencies
	existing_bump?: BumpType; // The bump type from existing changesets
	required_bump?: BumpType; // The required bump type from dependencies
}

export interface DependencyUpdate {
	dependent_package: string;
	updated_dependency: string;
	current_version: string;
	new_version: string;
	type: 'dependencies' | 'devDependencies' | 'peerDependencies';
	causes_republish: boolean;
}

// Verbose data types for diagnostic output
export interface VerboseChangesetDetail {
	package_name: string;
	files: Array<{filename: string; bump_type: BumpType; summary: string}>;
}

export interface VerboseIterationPackage {
	name: string;
	changeset_count: number;
	bump_from_changesets: BumpType | null;
	required_bump: BumpType | null;
	triggering_dep: string | null;
	action: 'publish' | 'auto_changeset' | 'escalation' | 'skip';
	version_to: string | null;
	is_breaking: boolean;
}

export interface VerboseIteration {
	iteration: number;
	packages: Array<VerboseIterationPackage>;
	new_changes: number;
}

export interface VerbosePropagationChain {
	source: string;
	chain: Array<{pkg: string; dep_type: 'prod' | 'peer'; action: string}>;
}

export interface VerboseGraphSummary {
	package_count: number;
	internal_dep_count: number;
	prod_peer_edges: Array<{from: string; to: string; type: 'prod' | 'peer'}>;
	dev_edges: Array<{from: string; to: string}>;
	prod_cycle_count: number;
	dev_cycle_count: number;
}

export interface VerboseData {
	changeset_details: Array<VerboseChangesetDetail>;
	iterations: Array<VerboseIteration>;
	propagation_chains: Array<VerbosePropagationChain>;
	graph_summary: VerboseGraphSummary;
	total_iterations: number;
}

export interface PublishingPlan {
	publishing_order: Array<string>;
	version_changes: Array<VersionChange>;
	dependency_updates: Array<DependencyUpdate>;
	breaking_cascades: Map<string, Array<string>>;
	warnings: Array<string>;
	info: Array<string>; // Informational status (not warnings)
	errors: Array<string>;
	verbose_data?: VerboseData;
}

export interface GeneratePlanOptions {
	log?: Logger;
	ops?: ChangesetOperations;
	verbose?: boolean;
}

/**
 * Generates a publishing plan showing what would happen during publishing.
 * Shows version changes, dependency updates, and breaking change cascades.
 * Uses fixed-point iteration to resolve transitive cascades.
 */
export const generate_publishing_plan = async (
	repos: Array<LocalRepo>,
	options: GeneratePlanOptions = {},
): Promise<PublishingPlan> => {
	const {log, ops = default_changeset_operations, verbose} = options;
	log?.info(st('cyan', 'Generating publishing plan...'));

	const warnings: Array<string> = [];
	const info: Array<string> = []; // Informational status (not warnings)
	const errors: Array<string> = [];

	// Verbose data collection
	const verbose_changeset_details: Array<VerboseChangesetDetail> = [];
	const verbose_iterations: Array<VerboseIteration> = [];

	// Build dependency graph and validate
	let publishing_order: Array<string>;
	let production_cycles: Array<Array<string>>;
	let dev_cycles: Array<Array<string>>;
	let graph: DependencyGraph | null = null;

	try {
		const validation = validate_dependency_graph(repos, {
			throw_on_prod_cycles: false, // Collect errors instead of throwing
			log_cycles: false, // We'll handle our own error collection
			log_order: false, // Plan generation doesn't need to log order
		});
		publishing_order = validation.publishing_order;
		production_cycles = validation.production_cycles;
		dev_cycles = validation.dev_cycles;
		graph = validation.graph; // Store for verbose output

		// Add topological sort error if present
		if (validation.sort_error) {
			errors.push(validation.sort_error);
		}
	} catch (error) {
		errors.push(`Failed to validate dependency graph: ${error}`);
		return {
			publishing_order: [],
			version_changes: [],
			dependency_updates: [],
			breaking_cascades: new Map(),
			warnings,
			info,
			errors,
		};
	}

	// Collect cycle errors
	if (production_cycles.length > 0) {
		for (const cycle of production_cycles) {
			errors.push(`Production dependency cycle: ${cycle.join(' → ')}`);
		}
	}

	// Dev cycles are shown in gitops_analyze, not repeated here
	if (dev_cycles.length > 0) {
		info.push(
			`${dev_cycles.length} dev dependency cycle(s) detected (normal, shown in gitops_analyze)`,
		);
	}

	// Initial pass: get all packages with explicit changesets
	const predicted_versions: Map<string, string> = new Map();
	const breaking_packages: Set<string> = new Set();
	const version_changes: Array<VersionChange> = [];

	for (const pkg_name of publishing_order) {
		const repo = repos.find((r) => r.library.name === pkg_name);
		if (!repo) continue;

		// Check for changesets
		const has_result = await ops.has_changesets({repo}); // eslint-disable-line no-await-in-loop

		if (!has_result.ok) {
			errors.push(`Failed to check changesets for ${pkg_name}: ${has_result.message}`);
			continue;
		}

		if (has_result.value) {
			// Predict version from changesets
			const prediction = await ops.predict_next_version({repo, log}); // eslint-disable-line no-await-in-loop

			if (!prediction) {
				// No changesets found - this shouldn't happen since has_changesets returned true
				continue;
			}

			if (!prediction.ok) {
				errors.push(`Failed to predict version for ${pkg_name}: ${prediction.message}`);
				continue;
			}

			// Capture changeset details for verbose output
			if (verbose) {
				const changesets_result = await ops.read_changesets({repo, log}); // eslint-disable-line no-await-in-loop
				if (changesets_result.ok) {
					const files = changesets_result.value
						.filter((cs) => cs.packages.some((p) => p.name === pkg_name))
						.map((cs) => {
							const pkg_entry = cs.packages.find((p) => p.name === pkg_name);
							return {
								filename: cs.filename,
								bump_type: pkg_entry?.bump_type || prediction.bump_type,
								summary: cs.summary,
							};
						});
					if (files.length > 0) {
						verbose_changeset_details.push({package_name: pkg_name, files});
					}
				}
			}

			{
				const old_version = repo.library.package_json.version || '0.0.0';
				const is_breaking = is_breaking_change(old_version, prediction.bump_type);

				predicted_versions.set(pkg_name, prediction.version);

				if (is_breaking) {
					breaking_packages.add(pkg_name);
				}

				version_changes.push({
					package_name: pkg_name,
					from: old_version,
					to: prediction.version,
					bump_type: prediction.bump_type,
					breaking: is_breaking,
					has_changesets: true,
				});
			}
		}
	}

	// Fixed-point iteration to resolve transitive cascades
	// Loop until no new version changes are discovered
	let iteration = 0;
	let changed = true;

	while (changed && iteration < GITOPS_MAX_ITERATIONS_DEFAULT) {
		changed = false;
		iteration++;

		// Verbose iteration tracking
		const verbose_iteration_packages: Array<VerboseIterationPackage> = [];
		let verbose_new_changes = 0;

		// Recalculate dependency updates based on current predicted versions
		// (breaking_cascades not needed during iteration, only calculated at the end)
		const {dependency_updates} = calculate_dependency_updates(
			repos,
			predicted_versions,
			breaking_packages,
		);

		// Process packages to check for bump escalation and auto-generated changesets
		for (const repo of repos) {
			const pkg_name = repo.library.name;

			// Get required bump from dependencies
			const required_bump = get_required_bump_for_dependencies(
				repo,
				dependency_updates,
				breaking_packages,
			);

			// Find triggering dependency for verbose output
			let triggering_dep: string | null = null;
			if (required_bump) {
				const relevant_updates = dependency_updates.filter(
					(u) =>
						u.dependent_package === pkg_name &&
						(u.type === 'dependencies' || u.type === 'peerDependencies') &&
						breaking_packages.has(u.updated_dependency),
				);
				if (relevant_updates.length > 0) {
					triggering_dep = `${relevant_updates[0]!.updated_dependency} BREAKING`;
				}
			}

			// Check if already in version_changes (has changesets)
			const existing_entry = version_changes.find((vc) => vc.package_name === pkg_name);

			if (existing_entry) {
				// Package has changesets - check if it needs bump escalation
				if (required_bump && compare_bump_types(required_bump, existing_entry.bump_type) > 0) {
					// Dependencies require a larger bump than existing changesets provide
					const old_version = repo.library.package_json.version || '0.0.0';
					const new_version = calculate_next_version(old_version, required_bump);

					// Only mark as changed if version actually changed
					if (existing_entry.to !== new_version) {
						changed = true;
						verbose_new_changes++;

						// Capture verbose data before updating
						if (verbose) {
							const changeset_detail = verbose_changeset_details.find(
								(d) => d.package_name === pkg_name,
							);
							verbose_iteration_packages.push({
								name: pkg_name,
								changeset_count: changeset_detail?.files.length || 1,
								bump_from_changesets: existing_entry.bump_type,
								required_bump,
								triggering_dep,
								action: 'escalation',
								version_to: new_version,
								is_breaking: is_breaking_change(old_version, required_bump),
							});
						}

						existing_entry.needs_bump_escalation = true;
						existing_entry.existing_bump = existing_entry.bump_type;
						existing_entry.required_bump = required_bump;
						existing_entry.bump_type = required_bump;
						existing_entry.to = new_version;
						existing_entry.breaking = is_breaking_change(old_version, required_bump);

						// Update predicted versions
						predicted_versions.set(pkg_name, new_version);

						if (existing_entry.breaking) {
							breaking_packages.add(pkg_name);
						}
					}
				}
			} else if (required_bump) {
				// No existing changesets but needs changeset for dependency updates
				const old_version = repo.library.package_json.version || '0.0.0';
				const new_version = calculate_next_version(old_version, required_bump);

				// Check if this is a new version (not already in version_changes)
				if (!predicted_versions.has(pkg_name)) {
					changed = true;
					verbose_new_changes++;

					const is_breaking = is_breaking_change(old_version, required_bump);

					// Capture verbose data
					if (verbose) {
						verbose_iteration_packages.push({
							name: pkg_name,
							changeset_count: 0,
							bump_from_changesets: null,
							required_bump,
							triggering_dep,
							action: 'auto_changeset',
							version_to: new_version,
							is_breaking,
						});
					}

					if (is_breaking) {
						breaking_packages.add(pkg_name);
					}

					version_changes.push({
						package_name: pkg_name,
						from: old_version,
						to: new_version,
						bump_type: required_bump,
						breaking: is_breaking,
						has_changesets: false,
						will_generate_changeset: true,
					});

					// Update predicted versions
					predicted_versions.set(pkg_name, new_version);
				}
			}
		}

		// Store verbose iteration data
		if (verbose && (verbose_iteration_packages.length > 0 || iteration === 1)) {
			verbose_iterations.push({
				iteration,
				packages: verbose_iteration_packages,
				new_changes: verbose_new_changes,
			});
		}
	}

	// Check if we hit iteration limit without convergence
	if (iteration === GITOPS_MAX_ITERATIONS_DEFAULT && changed) {
		// Calculate how many packages still need processing
		const pending_packages: Array<string> = [];

		// Recalculate one more time to see what's pending
		const {dependency_updates: pending_updates} = calculate_dependency_updates(
			repos,
			predicted_versions,
			breaking_packages,
		);

		for (const repo of repos) {
			const pkg_name = repo.library.name;
			const required_bump = get_required_bump_for_dependencies(
				repo,
				pending_updates,
				breaking_packages,
			);

			// Check if this package would need processing
			const existing_entry = version_changes.find((vc) => vc.package_name === pkg_name);
			const needs_escalation =
				existing_entry &&
				required_bump &&
				compare_bump_types(required_bump, existing_entry.bump_type) > 0;
			const needs_auto_changeset = !existing_entry && required_bump;

			if (needs_escalation || needs_auto_changeset) {
				pending_packages.push(pkg_name);
			}
		}

		// Add warning with diagnostics
		const pending_count = pending_packages.length;
		const estimated_iterations = Math.ceil(pending_count / 2); // Rough estimate
		warnings.push(
			`Reached maximum iterations (${GITOPS_MAX_ITERATIONS_DEFAULT}) without full convergence - ` +
				`${pending_count} package(s) may still need processing: ${pending_packages.join(', ')}. ` +
				`Estimated ${estimated_iterations} more iteration(s) needed.`,
		);
	}

	// Final dependency updates calculation after convergence
	const {dependency_updates, breaking_cascades} = calculate_dependency_updates(
		repos,
		predicted_versions,
		breaking_packages,
	);

	// Identify packages with no changes
	for (const repo of repos) {
		const has_version_change = version_changes.some((vc) => vc.package_name === repo.library.name);
		if (!has_version_change) {
			const has_result = await ops.has_changesets({repo}); // eslint-disable-line no-await-in-loop
			if (has_result.ok && !has_result.value) {
				info.push(repo.library.name);
			}
		}
	}

	// Build verbose data if requested
	let verbose_data: VerboseData | undefined;
	if (verbose) {
		// Build propagation chains from breaking_cascades
		const propagation_chains: Array<VerbosePropagationChain> = [];
		for (const [source, affected] of breaking_cascades) {
			const chain: Array<{pkg: string; dep_type: 'prod' | 'peer'; action: string}> = [];
			for (const pkg of affected) {
				// Determine dep type and action
				const update = dependency_updates.find(
					(u) => u.dependent_package === pkg && u.updated_dependency === source,
				);
				const dep_type: 'prod' | 'peer' = update?.type === 'peerDependencies' ? 'peer' : 'prod';
				const version_change = version_changes.find((vc) => vc.package_name === pkg);
				let action = 'update';
				if (version_change?.will_generate_changeset) {
					action = 'auto-changeset';
				} else if (version_change?.needs_bump_escalation) {
					action = 'bump escalation';
				}
				chain.push({pkg, dep_type, action});
			}
			if (chain.length > 0) {
				propagation_chains.push({source, chain});
			}
		}

		// Build graph summary
		const prod_peer_edges: Array<{from: string; to: string; type: 'prod' | 'peer'}> = [];
		const dev_edges: Array<{from: string; to: string}> = [];
		let internal_dep_count = 0;

		for (const [pkg_name, node] of graph.nodes) {
			for (const [dep_name, spec] of node.dependencies) {
				// Only count internal dependencies (deps that are also in the graph)
				if (graph.nodes.has(dep_name)) {
					internal_dep_count++;
					if (spec.type === 'dev') {
						dev_edges.push({from: pkg_name, to: dep_name});
					} else {
						prod_peer_edges.push({
							from: pkg_name,
							to: dep_name,
							type: spec.type === 'peer' ? 'peer' : 'prod',
						});
					}
				}
			}
		}

		verbose_data = {
			changeset_details: verbose_changeset_details,
			iterations: verbose_iterations,
			propagation_chains,
			graph_summary: {
				package_count: graph.nodes.size,
				internal_dep_count,
				prod_peer_edges,
				dev_edges,
				prod_cycle_count: production_cycles.length,
				dev_cycle_count: dev_cycles.length,
			},
			total_iterations: iteration,
		};
	}

	return {
		publishing_order,
		version_changes,
		dependency_updates,
		breaking_cascades,
		warnings,
		info,
		errors,
		verbose_data,
	};
};
