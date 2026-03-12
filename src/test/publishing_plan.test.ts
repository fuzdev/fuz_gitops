import {test, expect} from 'vitest';

import type {LocalRepo} from '$lib/local_repo.js';
import {generate_publishing_plan} from '$lib/publishing_plan.js';
import type {ChangesetOperations} from '$lib/operations.js';
import {create_mock_repo} from './test_helpers.ts';

test('detects breaking change cascades', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0', deps: {'pkg-a': '0.1.0'}}),
		create_mock_repo({name: 'pkg-c', version: '0.3.0', deps: {'pkg-b': '0.2.0'}}),
	];

	// Mock changeset operations to simulate breaking changes
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'pkg-a',
		}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'pkg-a') {
				// Simulate a breaking change for pkg-a
				return {ok: true, version: '0.2.0', bump_type: 'minor' as const};
			}
			return null;
		},
	};

	const plan = await generate_publishing_plan(repos, {ops: mock_ops});

	// pkg-a should have a breaking change (0.x.x minor bump)
	expect(plan.version_changes.find((vc) => vc.package_name === 'pkg-a')?.breaking).toBe(true);

	// pkg-b should cascade the breaking change
	expect(plan.breaking_cascades.has('pkg-a')).toBe(true);
	expect(plan.breaking_cascades.get('pkg-a')).toContain('pkg-b');
});

test('handles bump escalation', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0', deps: {'pkg-a': '0.1.0'}}),
	];

	// Mock operations where pkg-a has breaking change and pkg-b has patch
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ok: true, value: true}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'pkg-a') {
				return {ok: true, version: '0.2.0', bump_type: 'minor' as const}; // breaking
			}
			if (options.repo.library.name === 'pkg-b') {
				return {ok: true, version: '0.2.1', bump_type: 'patch' as const}; // non-breaking
			}
			return null;
		},
	};

	const plan = await generate_publishing_plan(repos, {ops: mock_ops});

	// pkg-b should have bump escalation due to breaking dep
	const pkg_b_change = plan.version_changes.find((vc) => vc.package_name === 'pkg-b');
	expect(pkg_b_change?.needs_bump_escalation).toBe(true);
	expect(pkg_b_change?.required_bump).toBe('minor');
});

test('generates auto-changesets for dependency updates', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0'}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0', deps: {'pkg-a': '0.1.0'}}),
		create_mock_repo({name: 'pkg-c', version: '0.3.0', dev_deps: {'pkg-a': '0.1.0'}}), // devDep only
	];

	// Mock operations where only pkg-a has changesets
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'pkg-a',
		}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'pkg-a') {
				return {ok: true, version: '0.1.1', bump_type: 'patch' as const};
			}
			return null;
		},
	};

	const plan = await generate_publishing_plan(repos, {ops: mock_ops});

	// pkg-b should get auto-changeset for dependency update
	const pkg_b_change = plan.version_changes.find((vc) => vc.package_name === 'pkg-b');
	expect(pkg_b_change?.will_generate_changeset).toBe(true);
	expect(pkg_b_change?.has_changesets).toBe(false);

	// pkg-c should not get auto-changeset (dev dependency only)
	const pkg_c_change = plan.version_changes.find((vc) => vc.package_name === 'pkg-c');
	expect(pkg_c_change).toBeUndefined();
});

test('handles circular dev dependencies', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0', dev_deps: {'pkg-b': '0.2.0'}}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0', dev_deps: {'pkg-a': '0.1.0'}}),
	];

	// Mock operations with no changesets
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ok: true, value: false}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async () => null,
	};

	const plan = await generate_publishing_plan(repos, {ops: mock_ops});

	// Should have info about dev cycles (not warnings anymore)
	expect(plan.info.some((i) => i.includes('dev dependency cycle(s) detected'))).toBe(true);

	// Should still compute publishing order
	expect(plan.publishing_order.length).toBe(2);

	// Should not have errors
	expect(plan.errors.length).toBe(0);
});

test('detects production circular dependencies', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'pkg-a', version: '0.1.0', deps: {'pkg-b': '0.2.0'}}),
		create_mock_repo({name: 'pkg-b', version: '0.2.0', deps: {'pkg-a': '0.1.0'}}),
	];

	// Mock operations with no changesets
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ok: true, value: false}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async () => null,
	};

	const plan = await generate_publishing_plan(repos, {ops: mock_ops});

	// Should have errors for production cycles
	expect(plan.errors.some((e) => e.includes('Production dependency cycle'))).toBe(true);

	// Should not compute publishing order
	expect(plan.publishing_order.length).toBe(0);
});

test('warns when MAX_ITERATIONS reached without convergence', async () => {
	// Create a very deep dependency chain (12 levels) with breaking changes
	// This will require more than 10 iterations to fully propagate
	const repos: Array<LocalRepo> = [
		create_mock_repo({name: 'level-1', version: '0.1.0'}),
		create_mock_repo({name: 'level-2', version: '0.1.0', deps: {'level-1': '^0.1.0'}}),
		create_mock_repo({name: 'level-3', version: '0.1.0', deps: {'level-2': '^0.1.0'}}),
		create_mock_repo({name: 'level-4', version: '0.1.0', deps: {'level-3': '^0.1.0'}}),
		create_mock_repo({name: 'level-5', version: '0.1.0', deps: {'level-4': '^0.1.0'}}),
		create_mock_repo({name: 'level-6', version: '0.1.0', deps: {'level-5': '^0.1.0'}}),
		create_mock_repo({name: 'level-7', version: '0.1.0', deps: {'level-6': '^0.1.0'}}),
		create_mock_repo({name: 'level-8', version: '0.1.0', deps: {'level-7': '^0.1.0'}}),
		create_mock_repo({name: 'level-9', version: '0.1.0', deps: {'level-8': '^0.1.0'}}),
		create_mock_repo({name: 'level-10', version: '0.1.0', deps: {'level-9': '^0.1.0'}}),
		create_mock_repo({name: 'level-11', version: '0.1.0', deps: {'level-10': '^0.1.0'}}),
		create_mock_repo({name: 'level-12', version: '0.1.0', deps: {'level-11': '^0.1.0'}}),
	];

	// Mock operations: only level-1 has a changeset with breaking change
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'level-1',
		}),
		read_changesets: async () => ({ok: true, value: []}),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'level-1') {
				// Breaking change in 0.x (minor bump)
				return {ok: true, version: '0.2.0', bump_type: 'minor' as const};
			}
			return null;
		},
	};

	const plan = await generate_publishing_plan(repos, {ops: mock_ops});

	// Should have a warning about MAX_ITERATIONS
	const convergence_warning = plan.warnings.find((w) => w.includes('Reached maximum iterations'));
	expect(convergence_warning).toBeDefined();

	// Warning should include diagnostics
	expect(convergence_warning).toContain('package(s) may still need processing');
	expect(convergence_warning).toContain('Estimated');
	expect(convergence_warning).toContain('iteration(s) needed');

	// Should still have produced some version changes (just not all of them)
	expect(plan.version_changes.length).toBeGreaterThan(0);
	expect(plan.version_changes.length).toBeLessThan(repos.length); // Not all processed
});
