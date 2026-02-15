import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests detection of circular production/peer dependencies.
 * This is an ERROR condition - should be detected and blocked.
 *
 * Structure:
 * - pkg_a (peer) → pkg_b (production) → pkg_a (creates production cycle)
 *
 * This should produce an error and prevent publishing.
 */
export const circular_prod_deps_error: RepoFixtureSet = {
	name: 'circular_prod_deps_error',
	description:
		'Tests that circular production/peer dependencies are detected and reported as errors',

	repos: [
		// pkg_a: Has peer dependency on pkg_b
		{
			repo_name: 'pkg_a',
			repo_url: 'https://gitops.fuz.dev/test/pkg_a',
			package_json: {
				name: '@test/pkg_a',
				version: '1.0.0',
				peerDependencies: {
					'@test/pkg_b': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'feature.md',
					content: `---
"@test/pkg_a": minor
---

New feature in pkg_a`,
				},
			],
		},

		// pkg_b: Has production dependency on pkg_a (creates cycle!)
		{
			repo_name: 'pkg_b',
			repo_url: 'https://gitops.fuz.dev/test/pkg_b',
			package_json: {
				name: '@test/pkg_b',
				version: '1.0.0',
				dependencies: {
					'@test/pkg_a': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'fix.md',
					content: `---
"@test/pkg_b": patch
---

Bug fix in pkg_b`,
				},
			],
		},
	],

	expected_outcomes: {
		// Publishing order cannot be determined due to cycle
		publishing_order: [],

		// No version changes should occur when there's an error
		version_changes: [],

		breaking_cascades: {},

		warnings: [],

		// Should detect the circular dependency
		// Two errors are generated:
		// 1. Topological sort fails with the circular dependency
		// 2. Cycle detection reports the specific cycle
		errors: [
			'Failed to compute publishing order: Error: dependency cycle detected: @test/pkg_a -> @test/pkg_b -> @test/pkg_a',
			'Production dependency cycle: @test/pkg_a → @test/pkg_b → @test/pkg_a',
		],
	},
};
