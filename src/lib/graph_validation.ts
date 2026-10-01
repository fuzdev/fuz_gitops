/**
 * Dependency graph validation shared by the publishing plan and the analysis tasks.
 *
 * Builds the graph, detects cycles by type, and computes the publishing order,
 * reporting cycles and sort failures in the result rather than throwing, so
 * each caller decides how to surface them: the plan turns them into errors,
 * `gitops_analyze` and `gitops_validate` format them.
 *
 * See also: `dependency_graph.ts` for the core graph data structure and algorithms.
 *
 * @module
 */

import { DependencyGraph, type DependencyAnalysis } from './dependency_graph.ts';
import { repo_is_npm, type LocalRepo } from './local_repo.ts';

export interface GraphValidationResult {
	graph: DependencyGraph;
	publishing_order: Array<string>;
	production_cycles: Array<Array<string>>;
	dev_cycles: Array<Array<string>>;
	/** Why the topological sort failed, when it did; `publishing_order` is then empty. */
	sort_error?: string;
}

/**
 * Builds the dependency graph, detects cycles, and computes the publishing order
 * (prod/peer dependencies only, so dev cycles don't block it).
 *
 * Never throws on cycles: a production/peer cycle leaves `publishing_order` empty
 * and sets `sort_error`, and the caller reports it.
 *
 * @returns the graph, publishing order, and detected cycles
 */
export const validate_dependency_graph = (repos: Array<LocalRepo>): GraphValidationResult => {
	const graph = new DependencyGraph(repos);
	const { production_cycles, dev_cycles } = graph.detect_cycles_by_type();

	let publishing_order: Array<string>;
	let sort_error: string | undefined;
	try {
		publishing_order = graph.topological_sort(true); // exclude dev deps to break cycles
	} catch (error) {
		sort_error = 'Failed to compute publishing order: ' + error;
		publishing_order = [];
	}

	return {
		graph,
		publishing_order,
		production_cycles,
		dev_cycles,
		sort_error
	};
};

export interface RepoAnalysis {
	graph: DependencyGraph;
	analysis: DependencyAnalysis;
	/** Topological publishing order, or `null` when prod/peer cycles prevent ordering. */
	publishing_order: Array<string> | null;
}

/**
 * Builds the dependency graph and runs cycle/wildcard analysis, tolerating cycles
 * (reports rather than throws). The shared core of `gitops_analyze` and
 * `gitops_validate`, which format the result themselves.
 */
export const analyze_repos = (repos: Array<LocalRepo>): RepoAnalysis => {
	// only npm packages form the dependency graph; non-npm repos (e.g. cargo) are excluded
	const { graph, publishing_order } = validate_dependency_graph(repos.filter(repo_is_npm));
	return {
		graph,
		analysis: graph.analyze(),
		publishing_order: publishing_order.length > 0 ? publishing_order : null
	};
};
