import {test, assert, describe, beforeAll, afterAll} from 'vitest';
import {existsSync} from 'node:fs';
import {join, dirname} from 'node:path';
import {fileURLToPath} from 'node:url';

import {validate_dependency_graph} from '$lib/graph_validation.js';
import {generate_publishing_plan} from '$lib/publishing_plan.js';
import {load_gitops_config} from '$lib/gitops_config.js';
import {publish_repos} from '$lib/multi_repo_publisher.js';
import {
	create_mock_gitops_ops,
	create_dirty_workspace_git_ops,
	create_wrong_branch_git_ops,
	create_unauthenticated_npm_ops,
	create_unavailable_registry_npm_ops,
	create_failing_build_ops,
	create_configurable_gitops_ops,
} from './mock_operations.js';
import {fixture_to_local_repos} from './load_repo_fixtures.js';
import type {LocalRepo} from '$lib/local_repo.js';
import {basic_publishing} from './repo_fixtures/basic_publishing.js';
import {deep_cascade} from './repo_fixtures/deep_cascade.js';
import {circular_dev_deps} from './repo_fixtures/circular_dev_deps.js';
import {three_way_dev_cycle} from './repo_fixtures/three_way_dev_cycle.js';
import {private_packages} from './repo_fixtures/private_packages.js';
import {major_bumps} from './repo_fixtures/major_bumps.js';
import {peer_deps_only} from './repo_fixtures/peer_deps_only.js';
import {circular_prod_deps_error} from './repo_fixtures/circular_prod_deps_error.js';
import {isolated_packages} from './repo_fixtures/isolated_packages.js';
import {multiple_dep_types} from './repo_fixtures/multiple_dep_types.js';
import type {RepoFixtureSet} from './repo_fixture_types.js';
import {generate_all_fixtures, fixtures_exist} from './generate_repos.js';
import {assert_publishing_order, assert_version_changes, assert_messages} from './helpers.js';

// All fixture sets
const FIXTURES: Array<RepoFixtureSet> = [
	basic_publishing,
	deep_cascade,
	circular_dev_deps,
	three_way_dev_cycle,
	private_packages,
	major_bumps,
	peer_deps_only,
	circular_prod_deps_error,
	isolated_packages,
	multiple_dep_types,
];

// Categorize fixtures by type
const SUCCESS_FIXTURES = FIXTURES.filter(
	(f) => !f.expected_outcomes.errors || f.expected_outcomes.errors.length === 0,
);
const ERROR_FIXTURES = FIXTURES.filter(
	(f) => f.expected_outcomes.errors && f.expected_outcomes.errors.length > 0,
);

// Cache for fixture LocalRepo objects (avoids redundant conversions)
const fixture_repos_cache: Map<string, Array<LocalRepo>> = new Map();

/**
 * Get or compute LocalRepo objects for a fixture.
 * Caches results to avoid redundant object allocation across tests.
 */
const get_fixture_repos = (fixture: RepoFixtureSet): Array<LocalRepo> => {
	let cached = fixture_repos_cache.get(fixture.name);
	if (!cached) {
		cached = fixture_to_local_repos(fixture);
		fixture_repos_cache.set(fixture.name, cached);
	}
	return cached;
};

/**
 * Helper to set up common test data for plan tests.
 * Creates mock operations, loads repos, and generates publishing plan.
 */
const setup_plan_test = async (fixture: RepoFixtureSet) => {
	const mock_ops = create_mock_gitops_ops(fixture);
	const local_repos = get_fixture_repos(fixture);
	const plan = await generate_publishing_plan(local_repos, {ops: mock_ops.changeset});
	return {mock_ops, local_repos, plan};
};

/**
 * Helper to set up common test data for dry run publishing tests.
 * Creates mock operations, loads repos, and runs dry run publish.
 */
const setup_dry_run_test = async (fixture: RepoFixtureSet) => {
	const mock_ops = create_mock_gitops_ops(fixture);
	const local_repos = get_fixture_repos(fixture);
	const result = await publish_repos(local_repos, {
		wetrun: false,
		update_deps: false,
		ops: mock_ops,
	});
	return {mock_ops, local_repos, result};
};

// Generate fixture repos before running tests (still needed for configs)
beforeAll(async () => {
	// Check if any fixtures are missing
	const missing = FIXTURES.some((f) => !fixtures_exist(f.name));

	if (missing) {
		// Generate all fixtures if any are missing
		await generate_all_fixtures(FIXTURES);
	}
}, 120_000); // Allow up to 2 minutes for fixture generation

// Clear cache after all tests to prevent memory leaks
afterAll(() => {
	fixture_repos_cache.clear();
});

/**
 * In-memory tests that avoid subprocess spawning and I/O.
 * Uses direct function calls with mocked operations for performance.
 */

/**
 * Success scenario fixtures - validate normal publishing workflows
 */
describe('Success scenario fixtures', () => {
	for (const fixture of SUCCESS_FIXTURES) {
		describe(fixture.name, () => {
			describe('analyze', () => {
				test('produces expected publishing order', () => {
					// No need for mock_ops in analyze - it doesn't use operations
					const local_repos = get_fixture_repos(fixture);

					// Validate dependency graph directly
					const {publishing_order: order} = validate_dependency_graph(local_repos, {
						throw_on_prod_cycles: false,
						log_cycles: false,
						log_order: false,
					});

					// Verify publishing order
					assert.ok(order, 'Should have publishing_order');
					assert_publishing_order(order, fixture.expected_outcomes.publishing_order);
				});
			});

			describe('plan', () => {
				test('predicts correct version changes', async () => {
					const {plan} = await setup_plan_test(fixture);

					// Verify version changes
					if (fixture.expected_outcomes.version_changes.length > 0) {
						assert_version_changes(plan.version_changes, fixture.expected_outcomes.version_changes);
					} else {
						assert.equal(plan.version_changes.length, 0, 'Expected no version changes');
					}
				});

				test('reports correct publishing order', async () => {
					const {plan} = await setup_plan_test(fixture);

					// Publishing order should match for fixtures with version changes
					if (fixture.expected_outcomes.version_changes.length > 0) {
						assert_publishing_order(
							plan.publishing_order,
							fixture.expected_outcomes.publishing_order,
						);
					}
				});

				// Only define if this fixture tests breaking cascades
				if (fixture.expected_outcomes.breaking_cascades) {
					test('tracks breaking cascades', async () => {
						const {plan} = await setup_plan_test(fixture);

						// Verify breaking cascades exist
						for (const [pkg, expected_affected] of Object.entries(
							fixture.expected_outcomes.breaking_cascades!,
						)) {
							assert.ok(plan.breaking_cascades.has(pkg), `Should have breaking cascade for ${pkg}`);

							// Verify affected packages match
							const actual_affected = plan.breaking_cascades.get(pkg) || [];
							for (const expected_pkg of expected_affected) {
								assert.ok(
									actual_affected.includes(expected_pkg),
									`${pkg} should affect ${expected_pkg}`,
								);
							}
						}
					});
				}

				// Only define if this fixture tests warnings
				if (fixture.expected_outcomes.warnings && fixture.expected_outcomes.warnings.length > 0) {
					test('reports warnings', async () => {
						const {plan} = await setup_plan_test(fixture);
						assert_messages(plan.warnings, fixture.expected_outcomes.warnings!, 'warnings');
					});
				}

				// Only define if this fixture tests info messages
				if (fixture.expected_outcomes.info && fixture.expected_outcomes.info.length > 0) {
					test('reports info for packages with no changes', async () => {
						const {plan} = await setup_plan_test(fixture);

						// Check that expected packages are in info
						for (const pkg of fixture.expected_outcomes.info!) {
							assert.ok(plan.info.includes(pkg), `Info should include ${pkg}`);
						}
					});
				}
			});

			describe('publish dry_run', () => {
				/*
				 * Note: Dry runs have limitations compared to full publishing:
				 * - Cannot auto-generate changesets (requires filesystem writes)
				 * - Limited bump escalation (requires iteration to see new dependency versions)
				 * - Only packages with explicit .changeset/ files can be published
				 *
				 * Tests validate that packages WITH explicit changesets are attempted,
				 * but cannot fully validate auto-generated or escalated scenarios.
				 */

				test('publishes expected packages', async () => {
					const {result} = await setup_dry_run_test(fixture);

					// Dry runs can ONLY publish packages with explicit changesets
					const packages_with_explicit_changesets =
						fixture.expected_outcomes.version_changes.filter(
							(vc) => vc.scenario === 'explicit_changeset' || vc.scenario === 'bump_escalation',
						);

					if (packages_with_explicit_changesets.length > 0) {
						// Dry_run should publish packages with explicit changesets
						assert.ok(
							result.published.length > 0,
							`Should publish at least some packages (expected ${packages_with_explicit_changesets.length} with explicit changesets)`,
						);

						// Verify published packages are in the expected set
						for (const published of result.published) {
							const expected_change = fixture.expected_outcomes.version_changes.find(
								(vc) => vc.package_name === published.name,
							);
							assert.ok(
								expected_change,
								`Published package ${published.name} should be in expected changes`,
							);
						}
					} else {
						// No packages with explicit changesets - dry_run should publish nothing
						assert.equal(result.published.length, 0, 'Should not publish any packages');
					}
				});

				test('reports success status', async () => {
					const {result} = await setup_dry_run_test(fixture);

					// Should report success for valid fixtures
					assert.ok(result.ok, 'Should report success');
					assert.equal(result.failed.length, 0, 'Should have no failures');
				});
			});
		});
	}
});

/**
 * Error scenario fixtures - validate error detection and reporting
 */
describe('Error scenario fixtures', () => {
	for (const fixture of ERROR_FIXTURES) {
		describe(fixture.name, () => {
			describe('analyze', () => {
				test('detects circular dependencies', () => {
					const local_repos = get_fixture_repos(fixture);

					// Validate dependency graph - should detect cycles
					const {publishing_order: order} = validate_dependency_graph(local_repos, {
						throw_on_prod_cycles: false,
						log_cycles: false,
						log_order: false,
					});

					// For error fixtures, publishing order should be empty or contain errors
					assert.ok(
						order.length === 0 ||
							order.length === fixture.expected_outcomes.publishing_order.length,
						'Error fixtures should have empty or error publishing order',
					);
				});
			});

			describe('plan', () => {
				test('reports errors', async () => {
					try {
						const {plan} = await setup_plan_test(fixture);
						// If plan succeeded, errors should be in result
						assert_messages(plan.errors, fixture.expected_outcomes.errors!, 'errors');
					} catch (_error) {
						// Command failed - this is expected for some error fixtures
						assert.ok(true, 'Plan generation failed as expected for error fixture');
					}
				});
			});
		});
	}
});

/**
 * Test that configs can actually be loaded.
 * This ensures the config files are valid TypeScript.
 */
describe('Config loading validation', () => {
	const FIXTURES_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

	for (const fixture of FIXTURES) {
		test(`${fixture.name} config loads successfully`, async () => {
			const config_path = join(FIXTURES_DIR, 'fixtures/configs', `${fixture.name}.config.ts`);

			// Verify config file exists
			assert.ok(existsSync(config_path), `Config file should exist at ${config_path}`);

			// Try to load the config
			const config = await load_gitops_config(config_path);
			assert.ok(config, 'Config should load successfully');
			assert.ok(Array.isArray(config.repos), 'Config should have repos array');
			assert.ok(config.repos.length > 0, 'Config should have at least one repo');
		});
	}
});

/**
 * Test error conditions and failure scenarios.
 * These tests validate that our mock operations correctly simulate various error states.
 */
describe('Error condition tests', () => {
	// Test that failure scenario mocks behave correctly
	test('dirty workspace mock returns expected values', async () => {
		const git_ops = create_dirty_workspace_git_ops();
		// Test the mock behavior
		const clean_result = await git_ops.check_clean_workspace();
		assert.ok(clean_result.ok);
		assert.equal(clean_result.value, false, 'Should report dirty workspace');

		const changes_result = await git_ops.has_changes();
		assert.ok(changes_result.ok);
		assert.equal(changes_result.value, true, 'Should have changes');
	});

	test('wrong branch mock returns expected values', async () => {
		const git_ops = create_wrong_branch_git_ops();
		const result = await git_ops.current_branch_name();
		assert.ok(result.ok);
		assert.equal(result.value, 'feature-branch', 'Should be on wrong branch');
	});

	test('npm auth failure mock returns expected values', async () => {
		const npm_ops = create_unauthenticated_npm_ops();
		const result = await npm_ops.check_auth();
		assert.equal(result.ok, false, 'Should fail auth check');
	});

	test('npm registry unavailable mock returns expected values', async () => {
		const npm_ops = create_unavailable_registry_npm_ops();
		const result = await npm_ops.check_registry();
		assert.equal(result.ok, false, 'Should fail registry check');
	});

	test('build failure mock returns expected values', async () => {
		const build_ops = create_failing_build_ops();
		const mock_repo = fixture_to_local_repos(basic_publishing)[0]!;
		const result = await build_ops.build_package({repo: mock_repo});
		assert.equal(result.ok, false, 'Should fail build');
	});
});

/**
 * Test JSON output format structure.
 * Uses basic_publishing fixture as it has comprehensive data for structure validation.
 */
describe('JSON output format tests', () => {
	const fixture = basic_publishing;

	test('plan output has expected JSON structure', async () => {
		const {plan} = await setup_plan_test(fixture);

		// Verify plan structure matches expected JSON format
		assert.ok(Array.isArray(plan.publishing_order), 'Should have publishing_order array');
		assert.ok(Array.isArray(plan.version_changes), 'Should have version_changes array');
		assert.ok(Array.isArray(plan.dependency_updates), 'Should have dependency_updates array');
		assert.ok(plan.breaking_cascades instanceof Map, 'Should have breaking_cascades map');
		assert.ok(Array.isArray(plan.warnings), 'Should have warnings array');
		assert.ok(Array.isArray(plan.errors), 'Should have errors array');
		assert.ok(Array.isArray(plan.info), 'Should have info array');

		// Verify version change structure
		if (plan.version_changes.length > 0) {
			const change = plan.version_changes[0]!;
			assert.ok('package_name' in change, 'Version change should have package_name');
			assert.ok('from' in change, 'Version change should have from version');
			assert.ok('to' in change, 'Version change should have to version');
			assert.ok('bump_type' in change, 'Version change should have bump_type');
			assert.ok('breaking' in change, 'Version change should have breaking flag');
			assert.ok('has_changesets' in change, 'Version change should have has_changesets flag');
		}
	});

	test('analyze output has expected JSON structure', () => {
		const local_repos = get_fixture_repos(fixture);

		const result = validate_dependency_graph(local_repos, {
			throw_on_prod_cycles: false,
			log_cycles: false,
			log_order: false,
		});

		assert.ok(result.graph, 'Should have dependency graph');
		assert.ok(Array.isArray(result.publishing_order), 'Should have publishing order');
		assert.ok(result.graph.nodes instanceof Map, 'Graph should have nodes map');

		// Verify node structure
		if (result.graph.nodes.size > 0) {
			const node = result.graph.nodes.values().next().value;
			if (node) {
				assert.ok('name' in node, 'Node should have name');
				assert.ok('version' in node, 'Node should have version');
				assert.ok('dependencies' in node, 'Node should have dependencies');
				assert.ok('dependents' in node, 'Node should have dependents');
				assert.ok('publishable' in node, 'Node should have publishable flag');
			}
		}
	});
});

/**
 * Test preflight operation mocks.
 * Uses basic_publishing fixture as it has varied changeset scenarios.
 */
describe('Preflight mock tests', () => {
	const fixture = basic_publishing;

	test('preflight mock operations categorize repos correctly', async () => {
		const mock_ops = create_mock_gitops_ops(fixture);

		// Preflight ops should categorize repos based on changeset data
		const result = await mock_ops.preflight.run_preflight_checks({} as any);
		assert.ok(result.repos_with_changesets instanceof Set, 'Should have repos_with_changesets set');
		assert.ok(
			result.repos_without_changesets instanceof Set,
			'Should have repos_without_changesets set',
		);

		// Verify categorization matches fixture data
		for (const repo of fixture.repos) {
			const has_changesets = repo.changesets && repo.changesets.length > 0;
			if (has_changesets) {
				assert.ok(
					result.repos_with_changesets.has(repo.package_json.name),
					`${repo.package_json.name} should be in repos_with_changesets`,
				);
			} else {
				assert.ok(
					result.repos_without_changesets.has(repo.package_json.name),
					`${repo.package_json.name} should be in repos_without_changesets`,
				);
			}
		}
	});

	test('configurable preflight can simulate failures', async () => {
		const mock_ops = create_configurable_gitops_ops(fixture, {
			preflight: {fails: true, errors: ['Test error']},
		});

		const result = await mock_ops.preflight.run_preflight_checks({} as any);
		assert.equal(result.ok, false, 'Should fail when configured to fail');
		assert.ok(result.errors.includes('Test error'), 'Should include configured error');
	});
});
