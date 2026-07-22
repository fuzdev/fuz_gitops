import type { VersionChange } from '$lib/publishing_plan.ts';
import type { RepoFixtureExpectedVersionChange } from './repo_fixture_types.ts';

/**
 * Assert that publishing order matches expected order.
 */
export const assert_publishing_order = (actual: Array<string>, expected: Array<string>): void => {
	if (actual.length !== expected.length) {
		throw new Error(
			`Publishing order length mismatch: expected ${expected.length} packages, got ${actual.length}.\n` +
				`Expected: ${expected.join(', ')}\n` +
				`Actual: ${actual.join(', ')}`
		);
	}

	for (let i = 0; i < expected.length; i++) {
		if (actual[i] !== expected[i]) {
			throw new Error(
				`Publishing order mismatch at position ${i + 1}:\n` +
					`Expected: ${expected[i]}\n` +
					`Actual: ${actual[i]}\n` +
					`Full expected: ${expected.join(', ')}\n` +
					`Full actual: ${actual.join(', ')}`
			);
		}
	}
};

/**
 * Assert that version changes match expected changes.
 * Matches by package name, from/to versions, and scenario.
 */
export const assert_version_changes = (
	actual: Array<VersionChange>,
	expected: Array<RepoFixtureExpectedVersionChange>
): void => {
	// Create maps for easy lookup
	const actual_by_pkg = new Map(actual.map((vc) => [vc.package_name, vc]));
	const expected_by_pkg = new Map(expected.map((vc) => [vc.package_name, vc]));

	// Check that all expected version changes exist
	for (const [pkg_name, expected_change] of expected_by_pkg) {
		const actual_change = actual_by_pkg.get(pkg_name);

		if (!actual_change) {
			throw new Error(
				`Expected version change for package '${pkg_name}' not found.\n` +
					`Expected: ${expected_change.from} → ${expected_change.to} (${expected_change.scenario})`
			);
		}

		// Verify from/to versions
		if (actual_change.from !== expected_change.from) {
			throw new Error(
				`Version mismatch for package '${pkg_name}':\n` +
					`Expected from version: ${expected_change.from}\n` +
					`Actual from version: ${actual_change.from}`
			);
		}

		if (actual_change.to !== expected_change.to) {
			throw new Error(
				`Version mismatch for package '${pkg_name}':\n` +
					`Expected to version: ${expected_change.to}\n` +
					`Actual to version: ${actual_change.to}`
			);
		}

		// Verify scenario
		const actual_scenario = get_version_change_scenario(actual_change);
		if (actual_scenario !== expected_change.scenario) {
			throw new Error(
				`Scenario mismatch for package '${pkg_name}':\n` +
					`Expected scenario: ${expected_change.scenario}\n` +
					`Actual scenario: ${actual_scenario}`
			);
		}
	}

	// Check for unexpected version changes
	for (const [pkg_name] of actual_by_pkg) {
		if (!expected_by_pkg.has(pkg_name)) {
			throw new Error(
				`Unexpected version change for package '${pkg_name}' not in expected outcomes`
			);
		}
	}
};

/**
 * Determine the scenario for a version change based on flags.
 *
 * Scenarios are determined by whether the package has explicit changesets:
 * - auto_generated: No explicit changesets, will auto-generate due to dependency updates
 * - bump_escalation: Has explicit changesets, but dependencies require higher bump
 * - explicit_changeset: Has explicit changesets, normal bump
 */
const get_version_change_scenario = (
	vc: VersionChange
): 'explicit_changeset' | 'bump_escalation' | 'auto_generated' => {
	// Check has_changesets first - this is the primary discriminator
	if (!vc.has_changesets) {
		// No explicit changesets = auto-generated (even if escalation also needed)
		return 'auto_generated';
	}
	// Has explicit changesets - check if escalation needed
	if (vc.needs_bump_escalation) {
		return 'bump_escalation';
	}
	// Has explicit changesets, normal bump
	return 'explicit_changeset';
};

/**
 * Assert that warnings/errors/info contain expected messages.
 * Checks that all expected messages are present (allows additional ones).
 */
export const assert_messages = (
	actual: Array<string>,
	expected: Array<string>,
	message_type: 'warnings' | 'errors' | 'info'
): void => {
	for (const expected_msg of expected) {
		const found = actual.some((actual_msg) => actual_msg.includes(expected_msg));

		if (!found) {
			throw new Error(
				`Expected ${message_type} message not found:\n` +
					`Expected (substring): ${expected_msg}\n` +
					`Actual ${message_type}: ${actual.join(', ')}`
			);
		}
	}
};
