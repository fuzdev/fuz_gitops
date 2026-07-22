import { describe, test, assert } from 'vitest';

import { generate_publishing_plan, type PublishingPlan } from '$lib/publishing_plan.ts';
import { basic_publishing } from './repo_fixtures/basic_publishing.ts';
import { deep_cascade } from './repo_fixtures/deep_cascade.ts';
import { circular_dev_deps } from './repo_fixtures/circular_dev_deps.ts';
import { private_packages } from './repo_fixtures/private_packages.ts';
import { major_bumps } from './repo_fixtures/major_bumps.ts';
import { peer_deps_only } from './repo_fixtures/peer_deps_only.ts';
import { circular_prod_deps_error } from './repo_fixtures/circular_prod_deps_error.ts';
import { isolated_packages } from './repo_fixtures/isolated_packages.ts';
import { multiple_dep_types } from './repo_fixtures/multiple_dep_types.ts';
import { fixture_to_local_repos } from './load_repo_fixtures.ts';
import { create_mock_changeset_ops } from './mock_changeset_operations.ts';
import type { RepoFixtureSet, RepoFixtureExpectedVersionChange } from './repo_fixture_types.ts';

// All fixtures to test
const FIXTURES = [
	basic_publishing,
	deep_cascade,
	circular_dev_deps,
	private_packages,
	major_bumps,
	peer_deps_only,
	circular_prod_deps_error,
	isolated_packages,
	multiple_dep_types
];

/**
 * Validates version changes match expected outcomes
 */
const validate_version_changes = (
	plan: PublishingPlan,
	expected: Array<RepoFixtureExpectedVersionChange>,
	fixture_name: string
): void => {
	assert.equal(
		plan.version_changes.length,
		expected.length,
		`${fixture_name}: Correct number of version changes`
	);

	// Verify each version change
	for (const expected_change of expected) {
		const actual = plan.version_changes.find(
			(vc) => vc.package_name === expected_change.package_name
		);
		assert.ok(actual, `${fixture_name}: Found version change for ${expected_change.package_name}`);
		assert.equal(
			actual.from,
			expected_change.from,
			`${fixture_name}: ${expected_change.package_name} from version matches`
		);
		assert.equal(
			actual.to,
			expected_change.to,
			`${fixture_name}: ${expected_change.package_name} to version matches`
		);

		// Check scenario-specific flags
		switch (expected_change.scenario) {
			case 'explicit_changeset':
				assert.equal(actual.has_changesets, true);
				assert.equal(actual.will_generate_changeset, undefined);
				assert.equal(actual.needs_bump_escalation, undefined);
				break;
			case 'auto_generated':
				assert.equal(actual.has_changesets, false);
				assert.equal(actual.will_generate_changeset, true);
				break;
			case 'bump_escalation':
				assert.equal(actual.has_changesets, true);
				assert.equal(actual.needs_bump_escalation, true);
				break;
		}
	}
};

/**
 * Validates breaking cascades match expected outcomes
 */
const validate_breaking_cascades = (
	plan: PublishingPlan,
	expected: Record<string, Array<string>> | undefined,
	fixture_name: string
): void => {
	if (!expected) return;

	for (const [pkg, affected] of Object.entries(expected)) {
		const actual_affected = plan.breaking_cascades.get(pkg);
		assert.ok(actual_affected, `${fixture_name}: Found breaking cascade for ${pkg}`);
		assert.deepEqual(
			actual_affected.sort(),
			affected.sort(),
			`${fixture_name}: Breaking cascade for ${pkg} matches`
		);
	}
};

/**
 * Validates info messages match expected outcomes
 */
const validate_info = (
	plan: PublishingPlan,
	expected: Array<string> | undefined,
	fixture_name: string
): void => {
	if (!expected) return;

	assert.deepEqual(
		plan.info.sort(),
		expected.sort(),
		`${fixture_name}: Info messages match expected`
	);
};

/**
 * Validates warnings match expected outcomes
 */
const validate_warnings = (
	plan: PublishingPlan,
	expected: Array<string> | undefined,
	fixture_name: string
): void => {
	if (expected === undefined) return; // Don't validate if not specified

	assert.deepEqual(
		plan.warnings.sort(),
		expected.sort(),
		`${fixture_name}: Warnings match expected`
	);
};

/**
 * Validates errors match expected outcomes
 */
const validate_errors = (
	plan: PublishingPlan,
	expected: Array<string> | undefined,
	fixture_name: string
): void => {
	if (expected === undefined) {
		// If no expected errors specified, assert no errors
		assert.equal(plan.errors.length, 0, `${fixture_name}: No unexpected errors`);
	} else {
		assert.deepEqual(plan.errors.sort(), expected.sort(), `${fixture_name}: Errors match expected`);
	}
};

/**
 * Generic test for a repo fixture set
 */
const test_fixture = async (fixture: RepoFixtureSet): Promise<void> => {
	const repos = fixture_to_local_repos(fixture);
	const ops = create_mock_changeset_ops(fixture);

	const plan = await generate_publishing_plan(repos, { ops });

	// Check publishing order
	assert.deepEqual(
		plan.publishing_order,
		fixture.expected_outcomes.publishing_order,
		`${fixture.name}: Publishing order matches expected`
	);

	// Check version changes
	validate_version_changes(plan, fixture.expected_outcomes.version_changes, fixture.name);

	// Check breaking cascades
	validate_breaking_cascades(plan, fixture.expected_outcomes.breaking_cascades, fixture.name);

	// Check info messages
	validate_info(plan, fixture.expected_outcomes.info, fixture.name);

	// Check warnings
	validate_warnings(plan, fixture.expected_outcomes.warnings, fixture.name);

	// Check errors
	validate_errors(plan, fixture.expected_outcomes.errors, fixture.name);
};

// Run tests for all fixtures
describe('repo fixtures', () => {
	for (const fixture of FIXTURES) {
		test(fixture.name, async () => {
			await test_fixture(fixture);
		});
	}
});
