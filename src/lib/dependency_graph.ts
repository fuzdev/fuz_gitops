/**
 * Dependency graph data structure and algorithms for multi-repo publishing.
 *
 * Provides `DependencyGraph` class with topological sort (via `@fuzdev/fuz_util/sort.js`)
 * and cycle detection by dependency type.
 * For validation workflow and publishing order computation, see `graph_validation.ts`.
 *
 * @module
 */

import {EMPTY_OBJECT} from '@fuzdev/fuz_util/object.js';
import {topological_sort as topological_sort_generic} from '@fuzdev/fuz_util/sort.js';

import type {LocalRepo} from './local_repo.js';

export const DEPENDENCY_TYPE = {
	PROD: 'prod',
	PEER: 'peer',
	DEV: 'dev',
} as const;

export type DependencyType = (typeof DEPENDENCY_TYPE)[keyof typeof DEPENDENCY_TYPE];

export interface DependencySpec {
	type: DependencyType;
	version: string;
	resolved?: string;
}

export interface DependencyGraphJson {
	nodes: Array<{
		name: string;
		version: string;
		dependencies: Array<{name: string; spec: DependencySpec}>;
		dependents: Array<string>;
		publishable: boolean;
	}>;
	edges: Array<{from: string; to: string}>;
}

export interface DependencyNode {
	name: string;
	version: string;
	repo?: LocalRepo;
	dependencies: Map<string, DependencySpec>;
	dependents: Set<string>;
	publishable: boolean;
}

export class DependencyGraph {
	nodes: Map<string, DependencyNode>;
	edges: Map<string, Set<string>>; // pkg -> dependents

	constructor() {
		this.nodes = new Map();
		this.edges = new Map();
	}

	public init_from_repos(repos: Array<LocalRepo>): void {
		// First pass: create nodes
		for (const repo of repos) {
			const {library} = repo;
			const node: DependencyNode = {
				name: library.name,
				version: library.package_json.version || '0.0.0',
				repo,
				dependencies: new Map(),
				dependents: new Set(),
				publishable: !!library.package_json.private === false, // eslint-disable-line @typescript-eslint/no-unnecessary-boolean-literal-compare
			};

			// Extract dependencies
			const deps = library.package_json.dependencies || (EMPTY_OBJECT as Record<string, string>);
			const dev_deps =
				library.package_json.devDependencies || (EMPTY_OBJECT as Record<string, string>);
			const peer_deps =
				library.package_json.peerDependencies || (EMPTY_OBJECT as Record<string, string>);

			// Add dependencies, prioritizing prod/peer over dev
			// (if a package appears in multiple dep types, use the stronger constraint)
			for (const [name, version] of Object.entries(deps)) {
				node.dependencies.set(name, {type: DEPENDENCY_TYPE.PROD, version});
			}
			for (const [name, version] of Object.entries(peer_deps)) {
				node.dependencies.set(name, {type: DEPENDENCY_TYPE.PEER, version});
			}
			for (const [name, version] of Object.entries(dev_deps)) {
				// Only add dev deps if not already present as prod/peer
				if (!node.dependencies.has(name)) {
					node.dependencies.set(name, {type: DEPENDENCY_TYPE.DEV, version});
				}
			}

			this.nodes.set(library.name, node);
			this.edges.set(library.name, new Set());
		}

		// Second pass: build edges (dependents)
		for (const node of this.nodes.values()) {
			for (const [dep_name] of node.dependencies) {
				if (this.nodes.has(dep_name)) {
					// Internal dependency
					const dep_node = this.nodes.get(dep_name)!;
					dep_node.dependents.add(node.name);
					this.edges.get(dep_name)!.add(node.name);
				}
			}
		}
	}

	get_node(name: string): DependencyNode | undefined {
		return this.nodes.get(name);
	}

	get_dependents(name: string): Set<string> {
		return this.edges.get(name) || new Set();
	}

	get_dependencies(name: string): Map<string, DependencySpec> {
		const node = this.nodes.get(name);
		return node ? node.dependencies : new Map();
	}

	/**
	 * Computes topological sort order for dependency graph.
	 *
	 * Delegates to `@fuzdev/fuz_util/sort.js` for the sorting algorithm.
	 * Throws if cycles detected.
	 *
	 * @param exclude_dev if true, excludes dev dependencies to break cycles.
	 *   Publishing uses exclude_dev=true to handle circular dev deps.
	 * @returns array of package names in dependency order (dependencies before dependents)
	 * @throws {Error} if circular dependencies detected in included dependency types
	 */
	topological_sort(exclude_dev = false): Array<string> {
		const items = Array.from(this.nodes.values()).map((node) => ({
			id: node.name,
			depends_on: Array.from(node.dependencies.entries())
				.filter(([dep_name, spec]) => {
					if (exclude_dev && spec.type === DEPENDENCY_TYPE.DEV) return false;
					return this.nodes.has(dep_name);
				})
				.map(([dep_name]) => dep_name),
		}));
		const result = topological_sort_generic(items, 'package');
		if (!result.ok) {
			throw new Error(result.error);
		}
		return result.sorted.map((item) => item.id);
	}

	/**
	 * Detects circular dependencies, categorized by severity.
	 *
	 * Production/peer cycles prevent publishing (impossible to order packages).
	 * Dev cycles are normal (test utils, shared configs) and safely ignored.
	 *
	 * Uses DFS traversal with recursion stack to identify back edges.
	 * Deduplicates cycles using sorted cycle keys.
	 *
	 * @returns object with production_cycles (errors) and dev_cycles (info)
	 */
	detect_cycles_by_type(): {
		production_cycles: Array<Array<string>>;
		dev_cycles: Array<Array<string>>;
	} {
		const production_cycles = this.#find_cycles((spec) => spec.type !== DEPENDENCY_TYPE.DEV);
		const dev_cycles = this.#find_cycles((spec) => spec.type === DEPENDENCY_TYPE.DEV);
		return {production_cycles, dev_cycles};
	}

	/** DFS cycle detection following only edges that match the filter. */
	#find_cycles(include: (spec: DependencySpec) => boolean): Array<Array<string>> {
		const cycles: Array<Array<string>> = [];
		const visited: Set<string> = new Set();
		const rec_stack: Set<string> = new Set();

		const dfs = (name: string, path: Array<string>): void => {
			visited.add(name);
			rec_stack.add(name);
			path.push(name);

			const node = this.nodes.get(name);
			if (node) {
				for (const [dep_name, spec] of node.dependencies) {
					if (!include(spec)) continue;

					if (this.nodes.has(dep_name)) {
						if (!visited.has(dep_name)) {
							dfs(dep_name, [...path]);
						} else if (rec_stack.has(dep_name)) {
							const cycle_start = path.indexOf(dep_name);
							const cycle = path.slice(cycle_start).concat(dep_name);
							const cycle_key = [...cycle].sort().join(',');
							const exists = cycles.some((c) => [...c].sort().join(',') === cycle_key);
							if (!exists) {
								cycles.push(cycle);
							}
						}
					}
				}
			}

			rec_stack.delete(name);
		};

		for (const name of this.nodes.keys()) {
			if (!visited.has(name)) {
				dfs(name, []);
			}
		}

		return cycles;
	}

	toJSON(): DependencyGraphJson {
		const nodes = Array.from(this.nodes.values()).map((node) => ({
			name: node.name,
			version: node.version,
			dependencies: Array.from(node.dependencies.entries()).map(([name, spec]) => ({
				name,
				spec,
			})),
			dependents: Array.from(node.dependents),
			publishable: node.publishable,
		}));

		const edges: Array<{from: string; to: string}> = [];
		for (const [from, tos] of this.edges) {
			for (const to of tos) {
				edges.push({from, to});
			}
		}

		return {nodes, edges};
	}
}

/**
 * Builder for creating and analyzing dependency graphs.
 */
export class DependencyGraphBuilder {
	/**
	 * Constructs dependency graph from local repos.
	 *
	 * Two-pass algorithm: first creates nodes, then builds edges (dependents).
	 * Prioritizes prod/peer deps over dev deps when same package appears in
	 * multiple dependency types (stronger constraint wins).
	 *
	 * @returns fully initialized dependency graph with all nodes and edges
	 */
	build_from_repos(repos: Array<LocalRepo>): DependencyGraph {
		const graph = new DependencyGraph();
		graph.init_from_repos(repos);
		return graph;
	}

	/**
	 * Computes publishing order using topological sort with dev deps excluded.
	 *
	 * Excludes dev dependencies to break circular dev dependency cycles while
	 * preserving production/peer dependency ordering. This allows patterns like
	 * shared test utilities that depend on each other for development.
	 *
	 * @returns package names in safe publishing order (dependencies before dependents)
	 * @throws {Error} if production/peer cycles detected (cannot be resolved by exclusion)
	 */
	compute_publishing_order(graph: DependencyGraph): Array<string> {
		return graph.topological_sort(true); // Exclude dev dependencies
	}

	analyze(graph: DependencyGraph): {
		production_cycles: Array<Array<string>>;
		dev_cycles: Array<Array<string>>;
		wildcard_deps: Array<{pkg: string; dep: string; version: string}>;
		missing_peers: Array<{pkg: string; dep: string}>;
	} {
		const {production_cycles, dev_cycles} = graph.detect_cycles_by_type();
		const wildcard_deps: Array<{pkg: string; dep: string; version: string}> = [];
		const missing_peers: Array<{pkg: string; dep: string}> = [];

		for (const node of graph.nodes.values()) {
			for (const [dep_name, spec] of node.dependencies) {
				if (spec.version === '*') {
					wildcard_deps.push({pkg: node.name, dep: dep_name, version: spec.version});
				}
				if (spec.type === DEPENDENCY_TYPE.PEER && !graph.nodes.has(dep_name)) {
					// External peer dependency
					missing_peers.push({pkg: node.name, dep: dep_name});
				}
			}
		}

		return {production_cycles, dev_cycles, wildcard_deps, missing_peers};
	}
}
