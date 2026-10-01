import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { styleText as st } from 'node:util';

import { repo_is_npm, type LocalRepo } from './local_repo.ts';
import { validate_dependency_graph } from './graph_validation.ts';
import {
	type BumpType,
	is_breaking_change,
	compare_bump_types,
	calculate_next_version
} from './version_utils.ts';
import type { ChangesetOperations } from './operations.ts';
import { default_changeset_operations } from './operations_defaults.ts';
import { GITOPS_MAX_ITERATIONS_DEFAULT } from './gitops_constants.ts';
import type { DependencyGraph } from './dependency_graph.ts';
import {
	calculate_dependency_updates,
	get_required_bump_for_dependencies
} from './publishing_plan_helpers.ts';

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

/**
 * How a version change arises in the plan: from the package's own changesets
 * (`explicit`), from its changesets with the bump raised to what a breaking
 * dependency requires (`escalation`), or from a changeset the executor
 * generates for a dependency update (`auto`).
 */
export type VersionChangeKind = 'explicit' | 'escalation' | 'auto';

/**
 * Classifies a version change — the one classification the plan's logger,
 * markdown, and side-effect preview share. A change without changesets of its
 * own is `auto` even if its bump was raised.
 */
export const version_change_kind = (change: VersionChange): VersionChangeKind =>
	!change.has_changesets ? 'auto' : change.needs_bump_escalation ? 'escalation' : 'explicit';

export interface DependencyUpdate {
	dependent_package: string;
	updated_dependency: string;
	current_version: string;
	new_version: string;
	type: 'dependencies' | 'devDependencies' | 'peerDependencies';
}

// Verbose data types for diagnostic output
export interface VerboseChangesetDetail {
	package_name: string;
	files: Array<{ filename: string; bump_type: BumpType; summary: string }>;
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
	chain: Array<{ pkg: string; dep_type: 'prod' | 'peer'; action: string }>;
}

export interface VerboseGraphSummary {
	package_count: number;
	internal_dep_count: number;
	prod_peer_edges: Array<{ from: string; to: string; type: 'prod' | 'peer' }>;
	dev_edges: Array<{ from: string; to: string }>;
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
	/** Informational sentences, not warnings: excluded non-npm repos, dev dependency cycles. */
	info: Array<string>;
	/** Package names with no changesets and no version change — nothing to publish. */
	no_changes: Array<string>;
	errors: Array<string>;
	verbose_data?: VerboseData;
}

export interface GeneratePlanOptions {
	log?: Logger;
	ops?: ChangesetOperations;
	verbose?: boolean;
}

type ChangesetPresence = Awaited<ReturnType<ChangesetOperations['has_changesets']>>;

/**
 * The plan's working state, shared by the initial pass and the fixed-point
 * iteration that extends it.
 */
interface CascadeState {
	version_changes: Array<VersionChange>;
	predicted_versions: Map<string, string>;
	breaking_packages: Set<string>;
}

/**
 * Generates a publishing plan showing what would happen during publishing.
 * Shows version changes, dependency updates, and breaking change cascades.
 * Uses fixed-point iteration to resolve transitive cascades.
 */
export const generate_publishing_plan = async (
	all_repos: Array<LocalRepo>,
	options: GeneratePlanOptions = {}
): Promise<PublishingPlan> => {
	const { log, ops = default_changeset_operations, verbose = false } = options;
	log?.info(st('cyan', 'Generating publishing plan...'));

	const warnings: Array<string> = [];
	const info: Array<string> = [];
	const errors: Array<string> = [];

	// Publishing concerns only npm packages. Non-npm repos (e.g. Rust cargo repos) are
	// loaded for the dashboard but have no npm identity in the changeset cascade — drop them
	// before building the graph so they never appear in the plan or publishing order.
	const non_npm_repos = all_repos.filter((r) => !repo_is_npm(r));
	if (non_npm_repos.length > 0) {
		info.push(
			`Excluded ${non_npm_repos.length} non-npm repo(s) from publishing: ` +
				non_npm_repos.map((r) => r.library.name).join(', ')
		);
	}
	const repos = all_repos.filter(repo_is_npm);

	// Build dependency graph and validate; cycles and a failed sort are reported, never thrown
	const validation = validate_dependency_graph(repos);
	const { publishing_order, production_cycles, dev_cycles, graph } = validation;
	if (validation.sort_error) {
		errors.push(validation.sort_error);
	}

	for (const cycle of production_cycles) {
		errors.push(`Production dependency cycle: ${cycle.join(' → ')}`);
	}

	// Dev cycles are shown in gitops_analyze, not repeated here
	if (dev_cycles.length > 0) {
		info.push(
			`${dev_cycles.length} dev dependency cycle(s) detected (normal, shown in gitops_analyze)`
		);
	}

	// Checked once per repo; the initial pass and the no-changes list both read it.
	const changeset_presence: Map<string, ChangesetPresence> = new Map(
		await Promise.all(
			repos.map(async (repo) => [repo.library.name, await ops.has_changesets({ repo })] as const)
		)
	);

	const state: CascadeState = {
		version_changes: [],
		predicted_versions: new Map(),
		breaking_packages: new Set()
	};

	const { changeset_details, repos_without_bump } = await plan_initial_pass({
		repos,
		publishing_order,
		changeset_presence,
		state,
		ops,
		log,
		verbose,
		warnings,
		errors
	});

	const cascade = plan_resolve_cascades({ repos, state, changeset_details, verbose });

	// Worded after the cascades, which may give such a repo an auto-changeset
	for (const repo of repos_without_bump) {
		const publishes = state.version_changes.some((vc) => vc.package_name === repo.library.name);
		warnings.push(await describe_changesets_without_bump(repo, publishes, ops));
	}

	if (!cascade.converged) {
		const convergence_warning = plan_convergence_warning(repos, state);
		if (convergence_warning) warnings.push(convergence_warning);
	}

	// Final dependency updates calculation after convergence
	const { dependency_updates, breaking_cascades } = calculate_dependency_updates(
		repos,
		state.predicted_versions,
		state.breaking_packages
	);

	// Packages with nothing to publish: no version change and no changesets
	const no_changes: Array<string> = [];
	for (const repo of repos) {
		const name = repo.library.name;
		if (state.version_changes.some((vc) => vc.package_name === name)) continue;
		const presence = changeset_presence.get(name);
		if (presence?.ok && !presence.value) {
			no_changes.push(name);
		}
	}

	const verbose_data = verbose
		? plan_verbose_data({
				graph,
				version_changes: state.version_changes,
				dependency_updates,
				breaking_cascades,
				changeset_details,
				iterations: cascade.verbose_iterations,
				total_iterations: cascade.iterations,
				prod_cycle_count: production_cycles.length,
				dev_cycle_count: dev_cycles.length
			})
		: undefined;

	return {
		publishing_order,
		version_changes: state.version_changes,
		dependency_updates,
		breaking_cascades,
		warnings,
		info,
		no_changes,
		errors,
		verbose_data
	};
};

interface PlanInitialPassOptions {
	repos: Array<LocalRepo>;
	publishing_order: Array<string>;
	changeset_presence: Map<string, ChangesetPresence>;
	state: CascadeState;
	ops: ChangesetOperations;
	log: Logger | undefined;
	verbose: boolean;
	warnings: Array<string>;
	errors: Array<string>;
}

interface PlanInitialPassResult {
	/** The changeset details for verbose output, empty unless `verbose`. */
	changeset_details: Array<VerboseChangesetDetail>;
	/**
	 * Repos whose changeset files yield no bump for their package, warned on
	 * once the cascades settle whether each publishes.
	 */
	repos_without_bump: Array<LocalRepo>;
}

/**
 * The initial pass: a version change for each package with explicit changesets,
 * in publishing order.
 *
 * @mutates options.state - adds the explicit version changes, their predicted versions, and the breaking ones
 * @mutates options.warnings - private packages carrying changesets
 * @mutates options.errors - failed changeset checks and predictions
 */
const plan_initial_pass = async (
	options: PlanInitialPassOptions
): Promise<PlanInitialPassResult> => {
	const { repos, publishing_order, changeset_presence, state, ops, log, verbose } = options;
	const { warnings, errors } = options;
	const changeset_details: Array<VerboseChangesetDetail> = [];
	const repos_without_bump: Array<LocalRepo> = [];

	for (const pkg_name of publishing_order) {
		const repo = repos.find((r) => r.library.name === pkg_name);
		if (!repo) continue;
		const has_result = changeset_presence.get(pkg_name);
		if (!has_result) continue;

		// Private packages never publish — exclude them from version changes entirely (no
		// publish step, npm-wait, bump escalation, or auto-changeset). They keep their slot in
		// the topological order. Flag a private package that carries a changeset, since that
		// changeset can't be published.
		if (repo.package_json.private) {
			if (has_result.ok && has_result.value) {
				warnings.push(`${pkg_name} is private — its changeset(s) will not be published`);
			}
			continue;
		}

		if (!has_result.ok) {
			errors.push(`Failed to check changesets for ${pkg_name}: ${has_result.message}`);
			continue;
		}

		if (!has_result.value) continue;

		// Predict version from changesets
		const prediction = await ops.predict_next_version({ repo, log });

		if (!prediction) {
			// Changeset files exist, but none yields a bump for this package
			repos_without_bump.push(repo);
			continue;
		}

		if (!prediction.ok) {
			errors.push(`Failed to predict version for ${pkg_name}: ${prediction.message}`);
			continue;
		}

		// Capture changeset details for verbose output
		if (verbose) {
			const changesets_result = await ops.read_changesets({ repo, log });
			if (changesets_result.ok) {
				const files = changesets_result.value
					.filter((cs) => cs.packages.some((p) => p.name === pkg_name))
					.map((cs) => {
						const pkg_entry = cs.packages.find((p) => p.name === pkg_name);
						return {
							filename: cs.filename,
							bump_type: pkg_entry?.bump_type || prediction.bump_type,
							summary: cs.summary
						};
					});
				if (files.length > 0) {
					changeset_details.push({ package_name: pkg_name, files });
				}
			}
		}

		const old_version = repo.package_json.version || '0.0.0';
		const is_breaking = is_breaking_change(old_version, prediction.bump_type);

		state.predicted_versions.set(pkg_name, prediction.version);

		if (is_breaking) {
			state.breaking_packages.add(pkg_name);
		}

		state.version_changes.push({
			package_name: pkg_name,
			from: old_version,
			to: prediction.version,
			bump_type: prediction.bump_type,
			breaking: is_breaking,
			has_changesets: true
		});
	}

	return { changeset_details, repos_without_bump };
};

/**
 * Words the warning for a repo whose changeset files yield no bump for its
 * package. It takes no bump from them: it publishes only as the auto-generated
 * change a dependency update requires, and otherwise not at all.
 *
 * @param publishes - whether the settled plan has a version change for the repo
 */
const describe_changesets_without_bump = async (
	repo: LocalRepo,
	publishes: boolean,
	ops: ChangesetOperations
): Promise<string> => {
	const pkg_name = repo.library.name;
	const consequence = publishes
		? 'it takes no bump from them and publishes only as the auto-generated change a dependency update requires'
		: 'it will not publish';
	// no `log`: `predict_next_version` already logged any invalid files
	const read = await ops.read_changesets({ repo });
	if (!read.ok) {
		return `${pkg_name} has changeset files that could not be read (${read.message}) — ${consequence}`;
	}
	if (read.value.length === 0) {
		return `${pkg_name} has changeset files, but none parses — ${consequence}; check their frontmatter`;
	}
	return (
		`${pkg_name} has changeset files, but none that parses names ${pkg_name} — ${consequence}; ` +
		`check the package names in their frontmatter`
	);
};

interface PlanResolveCascadesOptions {
	repos: Array<LocalRepo>;
	state: CascadeState;
	changeset_details: Array<VerboseChangesetDetail>;
	verbose: boolean;
}

interface PlanCascadeResult {
	/** Iterations run, at most `GITOPS_MAX_ITERATIONS_DEFAULT`. */
	iterations: number;
	/** False when the last iteration still found changes. */
	converged: boolean;
	/** Per-iteration details, empty unless `verbose`. */
	verbose_iterations: Array<VerboseIteration>;
}

/**
 * Fixed-point iteration over the dependency updates the current predictions
 * imply: escalates explicit changes a breaking dependency outgrows, and adds or
 * raises auto-changeset changes for dependents, until a pass finds nothing new.
 *
 * @mutates options.state - escalates and adds version changes, with their predicted versions and breaking set
 */
const plan_resolve_cascades = (options: PlanResolveCascadesOptions): PlanCascadeResult => {
	const { repos, state, changeset_details, verbose } = options;
	const { version_changes, predicted_versions, breaking_packages } = state;
	const verbose_iterations: Array<VerboseIteration> = [];

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
		const { dependency_updates } = calculate_dependency_updates(
			repos,
			predicted_versions,
			breaking_packages
		);

		// Process packages to check for bump escalation and auto-generated changesets
		for (const repo of repos) {
			const pkg_name = repo.library.name;

			// Private packages are excluded from version changes (they never publish), so they
			// never escalate or auto-generate a changeset.
			if (repo.package_json.private) continue;

			// Get required bump from dependencies
			const required_bump = get_required_bump_for_dependencies(
				repo,
				dependency_updates,
				breaking_packages
			);
			if (!required_bump) continue;

			// Find triggering dependency for verbose output
			let triggering_dep: string | null = null;
			const relevant_updates = dependency_updates.filter(
				(u) =>
					u.dependent_package === pkg_name &&
					(u.type === 'dependencies' || u.type === 'peerDependencies') &&
					breaking_packages.has(u.updated_dependency)
			);
			if (relevant_updates.length > 0) {
				triggering_dep = `${relevant_updates[0]!.updated_dependency} BREAKING`;
			}

			const old_version = repo.package_json.version || '0.0.0';
			const new_version = calculate_next_version(old_version, required_bump);
			const is_breaking = is_breaking_change(old_version, required_bump);

			const existing_entry = version_changes.find((vc) => vc.package_name === pkg_name);

			if (existing_entry) {
				// Dependencies may require a larger bump than the entry has. Only mark as
				// changed if the version actually changes.
				if (compare_bump_types(required_bump, existing_entry.bump_type) <= 0) continue;
				if (existing_entry.to === new_version) continue;

				changed = true;
				verbose_new_changes++;

				// An auto-changeset entry raised by a dependency that turned breaking later in the
				// iteration (a dependent listed before its dependency) is still an auto-changeset:
				// the executor generates its changeset at the bump the plan settles on. Only an
				// entry with changesets of its own escalates.
				const kind = version_change_kind(existing_entry);

				if (verbose) {
					const changeset_detail = changeset_details.find((d) => d.package_name === pkg_name);
					verbose_iteration_packages.push({
						name: pkg_name,
						changeset_count: kind === 'auto' ? 0 : changeset_detail?.files.length || 1,
						bump_from_changesets: kind === 'auto' ? null : existing_entry.bump_type,
						required_bump,
						triggering_dep,
						action: kind === 'auto' ? 'auto_changeset' : 'escalation',
						version_to: new_version,
						is_breaking
					});
				}

				if (kind !== 'auto') {
					existing_entry.needs_bump_escalation = true;
					existing_entry.existing_bump ??= existing_entry.bump_type;
					existing_entry.required_bump = required_bump;
				}
				existing_entry.bump_type = required_bump;
				existing_entry.to = new_version;
				existing_entry.breaking = is_breaking;

				predicted_versions.set(pkg_name, new_version);
				if (is_breaking) {
					breaking_packages.add(pkg_name);
				}
			} else if (!predicted_versions.has(pkg_name)) {
				// No existing changesets but needs changeset for dependency updates
				changed = true;
				verbose_new_changes++;

				if (verbose) {
					verbose_iteration_packages.push({
						name: pkg_name,
						changeset_count: 0,
						bump_from_changesets: null,
						required_bump,
						triggering_dep,
						action: 'auto_changeset',
						version_to: new_version,
						is_breaking
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
					will_generate_changeset: true
				});

				predicted_versions.set(pkg_name, new_version);
			}
		}

		// Store verbose iteration data
		if (verbose && (verbose_iteration_packages.length > 0 || iteration === 1)) {
			verbose_iterations.push({
				iteration,
				packages: verbose_iteration_packages,
				new_changes: verbose_new_changes
			});
		}
	}

	return { iterations: iteration, converged: !changed, verbose_iterations };
};

/**
 * Words the warning for a plan that hit `GITOPS_MAX_ITERATIONS_DEFAULT` still
 * changing, naming the packages one more pass would change.
 *
 * @returns the warning, or `null` when one more pass would change nothing (the last pass converged it)
 */
const plan_convergence_warning = (repos: Array<LocalRepo>, state: CascadeState): string | null => {
	const { version_changes, predicted_versions, breaking_packages } = state;
	const pending_packages: Array<string> = [];

	// Recalculate one more time to see what's pending
	const { dependency_updates: pending_updates } = calculate_dependency_updates(
		repos,
		predicted_versions,
		breaking_packages
	);

	for (const repo of repos) {
		const pkg_name = repo.library.name;
		if (repo.package_json.private) continue; // private packages never publish
		const required_bump = get_required_bump_for_dependencies(
			repo,
			pending_updates,
			breaking_packages
		);
		if (!required_bump) continue;

		const existing_entry = version_changes.find((vc) => vc.package_name === pkg_name);
		if (!existing_entry || compare_bump_types(required_bump, existing_entry.bump_type) > 0) {
			pending_packages.push(pkg_name);
		}
	}

	if (pending_packages.length === 0) return null;

	return (
		`Reached maximum iterations (${GITOPS_MAX_ITERATIONS_DEFAULT}) without full convergence - ` +
		`${pending_packages.length} package(s) may still need processing: ${pending_packages.join(', ')}.`
	);
};

interface PlanVerboseDataOptions {
	graph: DependencyGraph;
	version_changes: Array<VersionChange>;
	dependency_updates: Array<DependencyUpdate>;
	breaking_cascades: Map<string, Array<string>>;
	changeset_details: Array<VerboseChangesetDetail>;
	iterations: Array<VerboseIteration>;
	total_iterations: number;
	prod_cycle_count: number;
	dev_cycle_count: number;
}

/**
 * Assembles the verbose diagnostics: propagation chains from the breaking
 * cascades and a summary of the dependency graph, beside the changeset and
 * iteration details the passes collected.
 */
const plan_verbose_data = (options: PlanVerboseDataOptions): VerboseData => {
	const { graph, version_changes, dependency_updates, breaking_cascades } = options;

	// Build propagation chains from breaking_cascades
	const propagation_chains: Array<VerbosePropagationChain> = [];
	for (const [source, affected] of breaking_cascades) {
		const chain: Array<{ pkg: string; dep_type: 'prod' | 'peer'; action: string }> = [];
		for (const pkg of affected) {
			// Determine dep type and action
			const update = dependency_updates.find(
				(u) => u.dependent_package === pkg && u.updated_dependency === source
			);
			const dep_type: 'prod' | 'peer' = update?.type === 'peerDependencies' ? 'peer' : 'prod';
			const version_change = version_changes.find((vc) => vc.package_name === pkg);
			const kind = version_change && version_change_kind(version_change);
			const action =
				kind === 'auto' ? 'auto-changeset' : kind === 'escalation' ? 'bump escalation' : 'update';
			chain.push({ pkg, dep_type, action });
		}
		if (chain.length > 0) {
			propagation_chains.push({ source, chain });
		}
	}

	// Build graph summary
	const prod_peer_edges: Array<{ from: string; to: string; type: 'prod' | 'peer' }> = [];
	const dev_edges: Array<{ from: string; to: string }> = [];
	let internal_dep_count = 0;

	for (const [pkg_name, node] of graph.nodes) {
		for (const [dep_name, spec] of node.dependencies) {
			// Only count internal dependencies (deps that are also in the graph)
			if (graph.nodes.has(dep_name)) {
				internal_dep_count++;
				if (spec.type === 'dev') {
					dev_edges.push({ from: pkg_name, to: dep_name });
				} else {
					prod_peer_edges.push({
						from: pkg_name,
						to: dep_name,
						type: spec.type === 'peer' ? 'peer' : 'prod'
					});
				}
			}
		}
	}

	return {
		changeset_details: options.changeset_details,
		iterations: options.iterations,
		propagation_chains,
		graph_summary: {
			package_count: graph.nodes.size,
			internal_dep_count,
			prod_peer_edges,
			dev_edges,
			prod_cycle_count: options.prod_cycle_count,
			dev_cycle_count: options.dev_cycle_count
		},
		total_iterations: options.total_iterations
	};
};
