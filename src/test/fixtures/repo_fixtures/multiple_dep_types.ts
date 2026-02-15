import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests packages with multiple dependency types to the same package.
 * A package can have both peerDependencies and devDependencies on the same package.
 *
 * This validates:
 * - Handling of multiple dependency types to same package
 * - Peer dependencies trigger republish
 * - Dev dependencies don't trigger republish
 * - Proper bump escalation with mixed dependency types
 */
export const multiple_dep_types: RepoFixtureSet = {
	name: 'multiple_dep_types',
	description:
		'Tests packages with both peer and dev dependencies on the same package (common plugin pattern)',

	repos: [
		// core: The main library
		{
			repo_name: 'core',
			repo_url: 'https://gitops.fuz.dev/test/core',
			package_json: {
				name: '@test/core',
				version: '2.0.0',
			},
			changesets: [
				{
					filename: 'breaking.md',
					content: `---
"@test/core": major
---

Breaking change in core`,
				},
			],
		},

		// plugin: Has BOTH peerDep and devDep on core
		// This is common - peer for runtime, dev for testing/types
		{
			repo_name: 'plugin',
			repo_url: 'https://gitops.fuz.dev/test/plugin',
			package_json: {
				name: '@test/plugin',
				version: '1.5.0',
				peerDependencies: {
					'@test/core': '^2.0.0',
				},
				devDependencies: {
					'@test/core': '^2.0.0', // Same package, different dep type
				},
			},
			changesets: [
				{
					filename: 'patch.md',
					content: `---
"@test/plugin": patch
---

Small fix in plugin`,
				},
			],
		},

		// adapter: Only devDep on core (doesn't use it at runtime)
		{
			repo_name: 'adapter',
			repo_url: 'https://gitops.fuz.dev/test/adapter',
			package_json: {
				name: '@test/adapter',
				version: '1.0.0',
				devDependencies: {
					'@test/core': '^2.0.0',
				},
			},
			// No changesets - dev dep only, shouldn't republish
		},
	],

	expected_outcomes: {
		// Note: adapter has only devDep on core (no prod/peer deps)
		// With exclude_dev=true in topological sort, adapter and core are both tier 0
		publishing_order: ['@test/core', '@test/adapter', '@test/plugin'],

		version_changes: [
			{
				package_name: '@test/core',
				from: '2.0.0',
				to: '3.0.0', // Major bump
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/plugin',
				from: '1.5.0',
				to: '2.0.0', // Escalated from patch to major due to peer dep breaking change
				scenario: 'bump_escalation',
			},
		],

		breaking_cascades: {
			'@test/core': ['@test/plugin'], // plugin has peer dep (breaking cascade)
			// adapter only has devDep, so no breaking cascade
		},

		// adapter has no changes (dev dep update doesn't trigger republish)
		info: ['@test/adapter'],

		warnings: [],
		errors: [],
	},
};
