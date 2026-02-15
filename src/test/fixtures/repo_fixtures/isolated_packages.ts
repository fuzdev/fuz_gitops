import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests isolated packages with no internal dependencies.
 * All packages are independent and can be published in any order.
 *
 * This validates:
 * - Publishing order with no dependencies (any order is valid)
 * - Multiple independent version bumps
 * - No breaking cascades
 * - Mix of explicit changesets and no changes
 */
export const isolated_packages: RepoFixtureSet = {
	name: 'isolated_packages',
	description: 'Tests packages with no internal dependencies (fully independent)',

	repos: [
		// util_a: Standalone utility with minor changeset
		{
			repo_name: 'util_a',
			repo_url: 'https://gitops.fuz.dev/test/util_a',
			package_json: {
				name: '@test/util_a',
				version: '1.5.0',
			},
			changesets: [
				{
					filename: 'feature.md',
					content: `---
"@test/util_a": minor
---

New utility function`,
				},
			],
		},

		// util_b: Standalone utility with patch changeset
		{
			repo_name: 'util_b',
			repo_url: 'https://gitops.fuz.dev/test/util_b',
			package_json: {
				name: '@test/util_b',
				version: '2.3.1',
			},
			changesets: [
				{
					filename: 'fix.md',
					content: `---
"@test/util_b": patch
---

Bug fix`,
				},
			],
		},

		// util_c: Standalone utility with major changeset
		{
			repo_name: 'util_c',
			repo_url: 'https://gitops.fuz.dev/test/util_c',
			package_json: {
				name: '@test/util_c',
				version: '3.0.0',
			},
			changesets: [
				{
					filename: 'breaking.md',
					content: `---
"@test/util_c": major
---

Breaking API change`,
				},
			],
		},

		// util_d: No changeset, no changes
		{
			repo_name: 'util_d',
			repo_url: 'https://gitops.fuz.dev/test/util_d',
			package_json: {
				name: '@test/util_d',
				version: '1.0.0',
			},
			// No changesets - should be in info section
		},
	],

	expected_outcomes: {
		// Publishing order: all packages are independent, so order is non-deterministic
		// but all should appear
		publishing_order: ['@test/util_d', '@test/util_c', '@test/util_b', '@test/util_a'],

		version_changes: [
			{
				package_name: '@test/util_a',
				from: '1.5.0',
				to: '1.6.0',
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/util_b',
				from: '2.3.1',
				to: '2.3.2',
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/util_c',
				from: '3.0.0',
				to: '4.0.0',
				scenario: 'explicit_changeset',
			},
		],

		// No breaking cascades - packages are independent
		breaking_cascades: {},

		// util_d has no changes
		info: ['@test/util_d'],

		warnings: [],
		errors: [],
	},
};
