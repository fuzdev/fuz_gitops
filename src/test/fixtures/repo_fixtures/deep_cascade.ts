import type { RepoFixtureSet } from '../repo_fixture_types.ts';

/**
 * Tests deep cascading updates through 4 levels of dependencies.
 * Validates multi-level dependency cascading with bump escalation.
 *
 * Dependency chain: leaf → branch → trunk → root
 * - leaf: Breaking change (minor bump)
 * - branch: Auto-changeset from leaf update
 * - trunk: Patch changeset escalates to minor due to branch update
 * - root: Auto-changeset from trunk update
 */
export const deep_cascade: RepoFixtureSet = {
	name: 'deep_cascade',
	description:
		'Tests 4-level deep dependency chain with cascading breaking changes and bump escalation',

	repos: [
		// leaf: Bottom of dependency tree, breaking change
		{
			repo_name: 'leaf',
			repo_url: 'https://gitops.fuz.dev/test/leaf',
			package_json: {
				name: '@test/leaf',
				version: '0.1.0'
			},
			changesets: [
				{
					filename: 'breaking.md',
					content: `---
"@test/leaf": minor
---

Breaking change in leaf package`
				}
			]
		},

		// branch: Depends on leaf, should get auto-changeset
		{
			repo_name: 'branch',
			repo_url: 'https://gitops.fuz.dev/test/branch',
			package_json: {
				name: '@test/branch',
				version: '0.1.0',
				dependencies: {
					'@test/leaf': '^0.1.0'
				}
			}
			// No changesets - should get auto-generated due to leaf breaking update
		},

		// trunk: Depends on branch (peer), patch changeset should escalate to minor
		{
			repo_name: 'trunk',
			repo_url: 'https://gitops.fuz.dev/test/trunk',
			package_json: {
				name: '@test/trunk',
				version: '0.1.0',
				peerDependencies: {
					'@test/branch': '^0.1.0'
				}
			},
			changesets: [
				{
					filename: 'patch.md',
					content: `---
"@test/trunk": patch
---

Small fix in trunk (will escalate to minor due to breaking peer dependency)`
				}
			]
		},

		// root: Top of tree, depends on trunk, should get auto-changeset
		{
			repo_name: 'root',
			repo_url: 'https://gitops.fuz.dev/test/root',
			package_json: {
				name: '@test/root',
				version: '0.1.0',
				dependencies: {
					'@test/trunk': '^0.1.0'
				}
			}
			// No changesets - should get auto-generated due to trunk update
		}
	],

	expected_outcomes: {
		// Topological order: leaf first, then dependencies in order
		publishing_order: ['@test/leaf', '@test/branch', '@test/trunk', '@test/root'],

		version_changes: [
			{
				package_name: '@test/leaf',
				from: '0.1.0',
				to: '0.2.0',
				scenario: 'explicit_changeset'
			},
			{
				package_name: '@test/branch',
				from: '0.1.0',
				to: '0.2.0', // Correctly gets breaking update from leaf
				scenario: 'auto_generated'
			},
			{
				package_name: '@test/trunk',
				from: '0.1.0',
				to: '0.2.0', // Correctly escalated from patch due to branch's breaking update
				scenario: 'bump_escalation'
			},
			{
				package_name: '@test/root',
				from: '0.1.0',
				to: '0.2.0', // Correctly gets breaking update from trunk
				scenario: 'auto_generated'
			}
		],

		breaking_cascades: {
			// Each entry shows DIRECT dependents with breaking changes, not transitive
			'@test/leaf': ['@test/branch'], // branch directly depends on leaf
			'@test/branch': ['@test/trunk'], // trunk directly depends on branch (peer dep)
			'@test/trunk': ['@test/root'] // root directly depends on trunk
		},

		warnings: [],
		errors: []
	}
};
