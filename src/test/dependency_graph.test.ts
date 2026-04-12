import {assert, describe, test} from 'vitest';
import {DependencyGraph, DependencyGraphBuilder} from '$lib/dependency_graph.js';
import {create_mock_repo} from './test_helpers.js';

describe('DependencyGraph', () => {
	describe('basic functionality', () => {
		test('creates nodes for all repos', () => {
			const repos = [create_mock_repo({name: 'package-a'}), create_mock_repo({name: 'package-b'})];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			assert.strictEqual(graph.nodes.size, 2);
			assert.ok(graph.get_node('package-a') !== undefined);
			assert.ok(graph.get_node('package-b') !== undefined);
		});

		test('sets publishable flag based on private field', () => {
			const repos = [
				create_mock_repo({name: 'public-pkg', version: '1.0.0'}),
				create_mock_repo({name: 'private-pkg', version: '1.0.0', private: true}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			assert.strictEqual(graph.get_node('public-pkg')?.publishable, true);
			assert.strictEqual(graph.get_node('private-pkg')?.publishable, false);
		});

		test('extracts dependencies by type', () => {
			const repo = create_mock_repo({
				name: 'main-pkg',
				version: '1.0.0',
				deps: {dep1: '^1.0.0'},
				dev_deps: {devDep1: '^2.0.0'},
				peer_deps: {peerDep1: '^3.0.0'},
			});

			const graph = new DependencyGraph();
			graph.init_from_repos([repo]);
			const node = graph.get_node('main-pkg')!;

			assert.deepEqual(node.dependencies.get('dep1'), {
				type: 'prod',
				version: '^1.0.0',
			});
			assert.deepEqual(node.dependencies.get('devDep1'), {
				type: 'dev',
				version: '^2.0.0',
			});
			assert.deepEqual(node.dependencies.get('peerDep1'), {
				type: 'peer',
				version: '^3.0.0',
			});
		});
	});

	describe('dependency relationships', () => {
		test('builds internal dependency relationships', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			const lib_node = graph.get_node('lib')!;
			const app_node = graph.get_node('app')!;

			assert.strictEqual(lib_node.dependents.has('app'), true);
			assert.deepEqual(app_node.dependencies.get('lib'), {
				type: 'prod',
				version: '^1.0.0',
			});
		});

		test('ignores external dependencies for relationships', () => {
			const repo = create_mock_repo({
				name: 'pkg',
				version: '1.0.0',
				deps: {
					'internal-dep': '^1.0.0',
					'external-dep': '^1.0.0',
				},
			});

			const internal_dep_repo = create_mock_repo({name: 'internal-dep', version: '1.0.0'});

			const graph = new DependencyGraph();
			graph.init_from_repos([repo, internal_dep_repo]);

			const pkg_node = graph.get_node('pkg')!;
			const internal_node = graph.get_node('internal-dep')!;

			// Internal dependency creates relationship
			assert.strictEqual(internal_node.dependents.has('pkg'), true);

			// Both dependencies are in the node's dependencies map
			assert.strictEqual(pkg_node.dependencies.has('internal-dep'), true);
			assert.strictEqual(pkg_node.dependencies.has('external-dep'), true);
		});

		test('prioritizes prod/peer deps over dev deps for same package', () => {
			// Test for bug fix: When same package appears in multiple dep types,
			// prod/peer should take priority (not be overwritten by dev)
			const repos = [
				create_mock_repo({name: 'core', version: '1.0.0'}),
				create_mock_repo({
					name: 'plugin',
					version: '1.0.0',
					deps: {core: '^1.0.0'}, // Prod dependency
					dev_deps: {core: '^1.0.0'}, // Dev dependency on SAME package
				}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			const plugin_node = graph.get_node('plugin')!;

			// Should use PROD type, not DEV (prod takes priority)
			assert.deepEqual(plugin_node.dependencies.get('core'), {
				type: 'prod',
				version: '^1.0.0',
			});

			// Verify topological sort correctly orders core before plugin
			const order = graph.topological_sort(true); // exclude_dev=true
			assert.ok(order.indexOf('core') < order.indexOf('plugin'));
		});

		test('prioritizes peer deps over dev deps for same package', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({
					name: 'adapter',
					version: '1.0.0',
					peer_deps: {lib: '^1.0.0'}, // Peer dependency
					dev_deps: {lib: '^1.0.0'}, // Dev dependency on SAME package
				}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			const adapter_node = graph.get_node('adapter')!;

			// Should use PEER type, not DEV (peer takes priority)
			assert.deepEqual(adapter_node.dependencies.get('lib'), {
				type: 'peer',
				version: '^1.0.0',
			});
		});
	});

	describe('topological_sort', () => {
		test('sorts simple dependency chain', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'middleware', version: '1.0.0', deps: {lib: '^1.0.0'}}),
				create_mock_repo({name: 'app', version: '1.0.0', deps: {middleware: '^1.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const order = graph.topological_sort();

			assert.deepEqual(order, ['lib', 'middleware', 'app']);
		});

		test('handles multiple independent packages', () => {
			const repos = [
				create_mock_repo({name: 'independent-a', version: '1.0.0'}),
				create_mock_repo({name: 'independent-b', version: '1.0.0'}),
				create_mock_repo({
					name: 'uses-both',
					version: '1.0.0',
					deps: {
						'independent-a': '^1.0.0',
						'independent-b': '^1.0.0',
					},
				}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const order = graph.topological_sort();

			// Independent packages should come first
			assert.ok(order.indexOf('independent-a') < order.indexOf('uses-both'));
			assert.ok(order.indexOf('independent-b') < order.indexOf('uses-both'));
		});

		test('excludes dev dependencies when requested', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app', version: '1.0.0', dev_deps: {lib: '^1.0.0'}}), // dev dependency
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			// With dev dependencies (would create order constraint)
			const order_with_dev = graph.topological_sort(false);
			assert.deepEqual(order_with_dev, ['lib', 'app']);

			// Without dev dependencies (no constraints)
			const order_without_dev = graph.topological_sort(true);
			assert.strictEqual(order_without_dev.length, 2);
			// Order can be either way since no prod dependencies
		});

		test('throws on circular dependencies', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);

			assert.throws(() => graph.topological_sort(), /cycle/);
		});

		test('handles complex dependency chains', () => {
			// Create a diamond dependency pattern: app -> [lib-a, lib-b] -> shared
			const repos = [
				create_mock_repo({name: 'shared', version: '1.0.0'}),
				create_mock_repo({name: 'lib-a', version: '1.0.0', deps: {shared: '^1.0.0'}}),
				create_mock_repo({name: 'lib-b', version: '1.0.0', deps: {shared: '^1.0.0'}}),
				create_mock_repo({
					name: 'app',
					version: '1.0.0',
					deps: {'lib-a': '^1.0.0', 'lib-b': '^1.0.0'},
				}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const order = graph.topological_sort();

			// shared must come before lib-a and lib-b
			assert.ok(order.indexOf('shared') < order.indexOf('lib-a'));
			assert.ok(order.indexOf('shared') < order.indexOf('lib-b'));
			// lib-a and lib-b must come before app
			assert.ok(order.indexOf('lib-a') < order.indexOf('app'));
			assert.ok(order.indexOf('lib-b') < order.indexOf('app'));
		});

		test('handles single package with no dependencies', () => {
			const repos = [create_mock_repo({name: 'standalone', version: '1.0.0'})];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const order = graph.topological_sort();

			assert.deepEqual(order, ['standalone']);
		});

		test('handles packages with only external dependencies', () => {
			const repos = [
				create_mock_repo({
					name: 'pkg-a',
					version: '1.0.0',
					deps: {lodash: '^4.0.0', react: '^18.0.0'},
				}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {express: '^4.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const order = graph.topological_sort();

			// Both packages can be published in any order since no internal deps
			assert.strictEqual(order.length, 2);
			assert.ok(order.includes('pkg-a'));
			assert.ok(order.includes('pkg-b'));
		});

		test('produces deterministic ordering across multiple runs', () => {
			// Test for deterministic sorting: Same input should always produce same output
			const repos = [
				create_mock_repo({name: 'zebra', version: '1.0.0'}),
				create_mock_repo({name: 'alpha', version: '1.0.0'}),
				create_mock_repo({name: 'beta', version: '1.0.0'}),
				create_mock_repo({name: 'gamma', version: '1.0.0', deps: {alpha: '^1.0.0'}}),
			];

			// Run topological sort multiple times
			const orders = [];
			for (let i = 0; i < 10; i++) {
				const graph = new DependencyGraph();
				graph.init_from_repos(repos);
				orders.push(graph.topological_sort(true));
			}

			// All orders should be identical (deterministic)
			const first_order = JSON.stringify(orders[0]);
			for (const order of orders) {
				assert.strictEqual(JSON.stringify(order), first_order);
			}

			// Verify alpha comes before gamma (dependency constraint)
			assert.ok(orders[0]!.indexOf('alpha') < orders[0]!.indexOf('gamma'));
		});
	});

	describe('detect_cycles_by_type', () => {
		test('separates production and dev cycles', () => {
			const repos = [
				// Production cycle
				create_mock_repo({name: 'prod-a', version: '1.0.0', deps: {'prod-b': '^1.0.0'}}),
				create_mock_repo({name: 'prod-b', version: '1.0.0', deps: {'prod-a': '^1.0.0'}}),
				// Dev cycle
				create_mock_repo({name: 'dev-a', version: '1.0.0', dev_deps: {'dev-b': '^1.0.0'}}),
				create_mock_repo({name: 'dev-b', version: '1.0.0', dev_deps: {'dev-a': '^1.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const {production_cycles, dev_cycles} = graph.detect_cycles_by_type();

			assert.strictEqual(production_cycles.length, 1);
			assert.ok(production_cycles[0]!.includes('prod-a'));
			assert.ok(production_cycles[0]!.includes('prod-b'));

			assert.strictEqual(dev_cycles.length, 1);
			assert.ok(dev_cycles[0]!.includes('dev-a'));
			assert.ok(dev_cycles[0]!.includes('dev-b'));
		});

		test('treats peer dependencies as production', () => {
			const repos = [
				create_mock_repo({name: 'peer-a', version: '1.0.0', peer_deps: {'peer-b': '^1.0.0'}}),
				create_mock_repo({name: 'peer-b', version: '1.0.0', peer_deps: {'peer-a': '^1.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const {production_cycles, dev_cycles} = graph.detect_cycles_by_type();

			assert.strictEqual(production_cycles.length, 1);
			assert.strictEqual(dev_cycles.length, 0);
		});

		test('handles mixed dependency types (no cycles)', () => {
			// a -> b (prod), b -> a (dev) - this is NOT a cycle in either analysis
			const repos = [
				create_mock_repo({name: 'mixed-a', version: '1.0.0', deps: {'mixed-b': '^1.0.0'}}), // prod dep
				create_mock_repo({name: 'mixed-b', version: '1.0.0', dev_deps: {'mixed-a': '^1.0.0'}}), // dev dep back
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const {production_cycles, dev_cycles} = graph.detect_cycles_by_type();

			// No production cycle (dev deps excluded) and no dev cycle (prod deps excluded)
			assert.strictEqual(production_cycles.length, 0);
			assert.strictEqual(dev_cycles.length, 0);
		});

		test('handles complex mixed scenarios (no cycles)', () => {
			// a -> b (prod), b -> c (peer), a -> c (dev) - no cycles in either analysis
			const repos = [
				create_mock_repo({
					name: 'complex-a',
					version: '1.0.0',
					deps: {'complex-b': '^1.0.0'},
					dev_deps: {'complex-c': '^1.0.0'},
				}),
				create_mock_repo({name: 'complex-b', version: '1.0.0', peer_deps: {'complex-c': '^1.0.0'}}),
				create_mock_repo({name: 'complex-c', version: '1.0.0'}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const {production_cycles, dev_cycles} = graph.detect_cycles_by_type();

			// No complete cycles in either analysis
			assert.strictEqual(production_cycles.length, 0);
			assert.strictEqual(dev_cycles.length, 0);
		});

		test('detects actual dev cycles', () => {
			// Real dev cycle: a -> b (dev), b -> a (dev)
			const repos = [
				create_mock_repo({
					name: 'dev-cycle-a',
					version: '1.0.0',
					dev_deps: {'dev-cycle-b': '^1.0.0'},
				}),
				create_mock_repo({
					name: 'dev-cycle-b',
					version: '1.0.0',
					dev_deps: {'dev-cycle-a': '^1.0.0'},
				}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const {production_cycles, dev_cycles} = graph.detect_cycles_by_type();

			assert.strictEqual(production_cycles.length, 0);
			assert.strictEqual(dev_cycles.length, 1);
			assert.ok(dev_cycles[0]!.includes('dev-cycle-a'));
			assert.ok(dev_cycles[0]!.includes('dev-cycle-b'));
		});
	});

	describe('helper methods', () => {
		test('get_dependents returns correct set', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app-1', version: '1.0.0', deps: {lib: '^1.0.0'}}),
				create_mock_repo({name: 'app-2', version: '1.0.0', deps: {lib: '^1.0.0'}}),
			];

			const graph = new DependencyGraph();
			graph.init_from_repos(repos);
			const dependents = graph.get_dependents('lib');

			assert.deepEqual(dependents, new Set(['app-1', 'app-2']));
		});

		test('get_dependencies returns correct map', () => {
			const repo = create_mock_repo({
				name: 'pkg',
				version: '1.0.0',
				deps: {dep1: '^1.0.0'},
				dev_deps: {dep2: '^2.0.0'},
			});
			const graph = new DependencyGraph();
			graph.init_from_repos([repo]);
			const dependencies = graph.get_dependencies('pkg');

			assert.strictEqual(dependencies.size, 2);
			assert.strictEqual(dependencies.get('dep1')?.type, 'prod');
			assert.strictEqual(dependencies.get('dep2')?.type, 'dev');
		});
	});
});

describe('DependencyGraphBuilder', () => {
	test('builds graph from repos', () => {
		const builder = new DependencyGraphBuilder();
		const repos = [create_mock_repo({name: 'test', version: '1.0.0'})];

		const graph = builder.build_from_repos(repos);

		assert.strictEqual(graph.nodes.size, 1);
	});

	test('computes publishing order excluding dev deps', () => {
		const builder = new DependencyGraphBuilder();
		const repos = [
			create_mock_repo({name: 'lib', version: '1.0.0'}),
			create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
		];

		const graph = builder.build_from_repos(repos);
		const order = builder.compute_publishing_order(graph);

		assert.deepEqual(order, ['lib', 'app']);
	});

	describe('analyze', () => {
		test('finds wildcard dependencies', () => {
			const builder = new DependencyGraphBuilder();
			const repos = [create_mock_repo({name: 'pkg', version: '1.0.0', deps: {dep: '*'}})];

			const graph = builder.build_from_repos(repos);
			const analysis = builder.analyze(graph);

			assert.deepEqual(analysis.wildcard_deps, [{pkg: 'pkg', dep: 'dep', version: '*'}]);
		});

		test('finds missing peer dependencies', () => {
			const builder = new DependencyGraphBuilder();
			const repos = [
				create_mock_repo({
					name: 'pkg',
					version: '1.0.0',
					peer_deps: {
						'external-peer': '^1.0.0',
					},
				}),
			];

			const graph = builder.build_from_repos(repos);
			const analysis = builder.analyze(graph);

			assert.deepEqual(analysis.missing_peers, [{pkg: 'pkg', dep: 'external-peer'}]);
		});

		test('separates cycles by type in analysis', () => {
			const builder = new DependencyGraphBuilder();
			const repos = [
				create_mock_repo({name: 'prod-a', version: '1.0.0', deps: {'prod-b': '^1.0.0'}}),
				create_mock_repo({name: 'prod-b', version: '1.0.0', deps: {'prod-a': '^1.0.0'}}),
				create_mock_repo({name: 'dev-a', version: '1.0.0', dev_deps: {'dev-b': '^1.0.0'}}),
				create_mock_repo({name: 'dev-b', version: '1.0.0', dev_deps: {'dev-a': '^1.0.0'}}),
			];

			const graph = builder.build_from_repos(repos);
			const analysis = builder.analyze(graph);

			assert.strictEqual(analysis.production_cycles.length, 1);
			assert.strictEqual(analysis.dev_cycles.length, 1);
		});
	});
});
