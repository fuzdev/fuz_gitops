import {describe, it, expect} from 'vitest';
import {TaskError} from '@fuzdev/gro';

import {validate_dependency_graph} from '$lib/graph_validation.js';
import {create_mock_repo} from './test_helpers.ts';

describe('validate_dependency_graph', () => {
	describe('basic functionality', () => {
		it('builds graph and returns publishing order for simple chain', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toEqual(['lib', 'app']);
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toEqual([]);
			expect(result.sort_error).toBeUndefined();
		});

		it('handles multiple independent packages', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0'}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0'}),
			];

			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toHaveLength(2);
			expect(result.publishing_order).toContain('pkg-a');
			expect(result.publishing_order).toContain('pkg-b');
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toEqual([]);
		});

		it('handles empty repos array', () => {
			const result = validate_dependency_graph([]);

			expect(result.publishing_order).toEqual([]);
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toEqual([]);
		});

		it('handles complex dependency diamond', () => {
			const repos = [
				create_mock_repo({name: 'base', version: '1.0.0'}),
				create_mock_repo({name: 'mid-a', version: '1.0.0', deps: {base: '^1.0.0'}}),
				create_mock_repo({name: 'mid-b', version: '1.0.0', deps: {base: '^1.0.0'}}),
				create_mock_repo({
					name: 'top',
					version: '1.0.0',
					deps: {'mid-a': '^1.0.0', 'mid-b': '^1.0.0'},
				}),
			];

			const result = validate_dependency_graph(repos);

			// base must come first, top must come last
			expect(result.publishing_order[0]).toBe('base');
			expect(result.publishing_order[3]).toBe('top');
			expect(result.production_cycles).toEqual([]);
		});
	});

	describe('production cycles with throw_on_prod_cycles=true', () => {
		it('throws on simple production cycle', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			expect(() => validate_dependency_graph(repos, {throw_on_prod_cycles: true})).toThrow(
				TaskError,
			);
			expect(() => validate_dependency_graph(repos, {throw_on_prod_cycles: true})).toThrow(
				/Cannot publish with production\/peer dependency cycles/,
			);
		});

		it('throws on peer dependency cycle', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', peer_deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', peer_deps: {'pkg-a': '^1.0.0'}}),
			];

			expect(() => validate_dependency_graph(repos, {throw_on_prod_cycles: true})).toThrow(
				TaskError,
			);
		});

		it('throws on mixed prod/peer cycle', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', peer_deps: {'pkg-a': '^1.0.0'}}),
			];

			expect(() => validate_dependency_graph(repos, {throw_on_prod_cycles: true})).toThrow(
				TaskError,
			);
		});

		it('throws on longer cycle (3+ packages)', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-c': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-c', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			expect(() => validate_dependency_graph(repos, {throw_on_prod_cycles: true})).toThrow(
				TaskError,
			);
		});
	});

	describe('production cycles with throw_on_prod_cycles=false', () => {
		it('returns empty order and sort_error for simple production cycle', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {throw_on_prod_cycles: false});

			expect(result.publishing_order).toEqual([]);
			expect(result.production_cycles).toHaveLength(1);
			expect(result.production_cycles[0]).toContain('pkg-a');
			expect(result.production_cycles[0]).toContain('pkg-b');
			expect(result.sort_error).toMatch(/Failed to compute publishing order/);
			expect(result.sort_error).toMatch(/Circular dependency/);
		});

		it('captures multiple production cycles', () => {
			const repos = [
				// Cycle 1
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
				// Cycle 2
				create_mock_repo({name: 'pkg-c', version: '1.0.0', deps: {'pkg-d': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-d', version: '1.0.0', deps: {'pkg-c': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {throw_on_prod_cycles: false});

			expect(result.publishing_order).toEqual([]);
			expect(result.production_cycles).toHaveLength(2);
			expect(result.sort_error).toBeDefined();
		});

		it('returns cycle info for peer dependency cycle', () => {
			const repos = [
				create_mock_repo({name: 'plugin-a', version: '1.0.0', peer_deps: {'plugin-b': '^1.0.0'}}),
				create_mock_repo({name: 'plugin-b', version: '1.0.0', peer_deps: {'plugin-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {throw_on_prod_cycles: false});

			expect(result.publishing_order).toEqual([]);
			expect(result.production_cycles).toHaveLength(1);
			expect(result.sort_error).toBeDefined();
		});
	});

	describe('dev cycles', () => {
		it('returns valid order with dev cycle present', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', dev_deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', dev_deps: {'pkg-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos);

			// Dev cycles don't block topological sort (dev deps excluded)
			expect(result.publishing_order).toHaveLength(2);
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toHaveLength(1);
			expect(result.dev_cycles[0]).toContain('pkg-a');
			expect(result.dev_cycles[0]).toContain('pkg-b');
			expect(result.sort_error).toBeUndefined();
		});

		it('handles multiple dev cycles', () => {
			const repos = [
				// Dev cycle 1
				create_mock_repo({name: 'test-a', version: '1.0.0', dev_deps: {'test-b': '^1.0.0'}}),
				create_mock_repo({name: 'test-b', version: '1.0.0', dev_deps: {'test-a': '^1.0.0'}}),
				// Dev cycle 2
				create_mock_repo({name: 'tool-a', version: '1.0.0', dev_deps: {'tool-b': '^1.0.0'}}),
				create_mock_repo({name: 'tool-b', version: '1.0.0', dev_deps: {'tool-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toHaveLength(4);
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toHaveLength(2);
		});

		it('computes order correctly ignoring dev deps', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({
					name: 'app',
					version: '1.0.0',
					deps: {lib: '^1.0.0'}, // prod dep
					dev_deps: {lib: '^1.0.0'}, // also dev dep (redundant but valid)
				}),
			];

			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toEqual(['lib', 'app']);
			expect(result.dev_cycles).toEqual([]);
		});
	});

	describe('mixed cycles', () => {
		it('separates production and dev cycles correctly', () => {
			const repos = [
				// Production cycle
				create_mock_repo({name: 'prod-a', version: '1.0.0', deps: {'prod-b': '^1.0.0'}}),
				create_mock_repo({name: 'prod-b', version: '1.0.0', deps: {'prod-a': '^1.0.0'}}),
				// Dev cycle
				create_mock_repo({name: 'dev-a', version: '1.0.0', dev_deps: {'dev-b': '^1.0.0'}}),
				create_mock_repo({name: 'dev-b', version: '1.0.0', dev_deps: {'dev-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {throw_on_prod_cycles: false});

			expect(result.production_cycles).toHaveLength(1);
			expect(result.dev_cycles).toHaveLength(1);
			expect(result.publishing_order).toEqual([]); // blocked by prod cycle
			expect(result.sort_error).toBeDefined();
		});

		it('handles mixed dep types without cycles', () => {
			// a -> b (prod), b -> a (dev) - NOT a cycle in either analysis
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', dev_deps: {'pkg-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos);

			// pkg-b must come first (pkg-a depends on it via prod dep)
			// The dev dep from pkg-b to pkg-a is ignored in topological sort
			expect(result.publishing_order).toEqual(['pkg-b', 'pkg-a']);
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toEqual([]);
		});
	});

	describe('logging options', () => {
		it('respects log_cycles=false (no crashes)', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			// Should not crash even though we're passing undefined logger
			const result = validate_dependency_graph(repos, {
				throw_on_prod_cycles: false,
				log_cycles: false,
			});

			expect(result.production_cycles).toHaveLength(1);
		});

		it('respects log_order=false (no crashes)', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {log_order: false});

			expect(result.publishing_order).toEqual(['lib', 'app']);
		});

		it('works with all logging disabled', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', dev_deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', dev_deps: {'pkg-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {
				throw_on_prod_cycles: false,
				log_cycles: false,
				log_order: false,
			});

			expect(result.dev_cycles).toHaveLength(1);
			expect(result.publishing_order).toHaveLength(2);
		});
	});

	describe('edge cases', () => {
		it('handles single package', () => {
			const repos = [create_mock_repo({name: 'solo', version: '1.0.0'})];

			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toEqual(['solo']);
			expect(result.production_cycles).toEqual([]);
			expect(result.dev_cycles).toEqual([]);
		});

		it('handles packages with only external dependencies', () => {
			const repos = [
				create_mock_repo({
					name: 'app',
					version: '1.0.0',
					deps: {react: '^18.0.0', lodash: '^4.0.0'},
				}),
			];

			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toEqual(['app']);
			expect(result.production_cycles).toEqual([]);
		});

		it('handles self-dependency (pathological)', () => {
			const repos = [
				create_mock_repo({name: 'self-dep', version: '1.0.0', deps: {'self-dep': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {throw_on_prod_cycles: false});

			expect(result.publishing_order).toEqual([]);
			expect(result.production_cycles).toHaveLength(1);
			expect(result.sort_error).toBeDefined();
		});

		it('handles private packages', () => {
			const repos = [
				create_mock_repo({name: 'private', version: '1.0.0', private: true}),
				create_mock_repo({name: 'public', version: '1.0.0', deps: {private: '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos);

			// Both included in order (graph_validation doesn't filter by private)
			expect(result.publishing_order).toEqual(['private', 'public']);
		});
	});

	describe('default options behavior', () => {
		it('defaults to throw_on_prod_cycles=true', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			// Default behavior should throw
			expect(() => validate_dependency_graph(repos)).toThrow(TaskError);
		});

		it('defaults to log_cycles=true (no crash with undefined logger)', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
			];

			// Should not crash with undefined logger even though log_cycles defaults to true
			const result = validate_dependency_graph(repos);

			expect(result.publishing_order).toEqual(['lib', 'app']);
		});
	});

	describe('graph property', () => {
		it('returns valid graph object', () => {
			const repos = [
				create_mock_repo({name: 'lib', version: '1.0.0'}),
				create_mock_repo({name: 'app', version: '1.0.0', deps: {lib: '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos);

			expect(result.graph).toBeDefined();
			expect(result.graph.nodes.size).toBe(2);
			expect(result.graph.get_node('lib')).toBeDefined();
			expect(result.graph.get_node('app')).toBeDefined();
		});

		it('graph contains cycle information', () => {
			const repos = [
				create_mock_repo({name: 'pkg-a', version: '1.0.0', deps: {'pkg-b': '^1.0.0'}}),
				create_mock_repo({name: 'pkg-b', version: '1.0.0', deps: {'pkg-a': '^1.0.0'}}),
			];

			const result = validate_dependency_graph(repos, {throw_on_prod_cycles: false});

			// Verify we can call graph methods
			const cycles = result.graph.detect_cycles();
			expect(cycles.length).toBeGreaterThan(0);
		});
	});
});
