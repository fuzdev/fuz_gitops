import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests major version transitions:
 * - 0.x.x → 1.0.0 (graduating from pre-1.0)
 * - 1.x.x → 2.0.0 (major version bump)
 *
 * Both transitions should propagate as breaking changes to dependents.
 */
export const major_bumps: RepoFixtureSet = {
	name: 'major_bumps',
	description: 'Tests major version transitions (0.x → 1.0 and 1.x → 2.0) with cascading updates',

	repos: [
		// unstable: Pre-1.0 package graduating to 1.0.0
		{
			repo_name: 'unstable',
			repo_url: 'https://gitops.fuz.dev/test/unstable',
			package_json: {
				name: '@test/unstable',
				version: '0.9.5',
			},
			changesets: [
				{
					filename: 'stable-release.md',
					content: `---
"@test/unstable": major
---

Graduating to 1.0.0 - stable release`,
				},
			],
		},

		// stable: Existing 1.x package getting major bump to 2.0.0
		{
			repo_name: 'stable',
			repo_url: 'https://gitops.fuz.dev/test/stable',
			package_json: {
				name: '@test/stable',
				version: '1.5.2',
			},
			changesets: [
				{
					filename: 'breaking.md',
					content: `---
"@test/stable": major
---

Breaking API changes - bumping to 2.0.0`,
				},
			],
		},

		// app_using_unstable: Depends on unstable (0.9.5 → 1.0.0)
		{
			repo_name: 'app_using_unstable',
			repo_url: 'https://gitops.fuz.dev/test/app_using_unstable',
			package_json: {
				name: '@test/app_using_unstable',
				version: '1.0.0',
				dependencies: {
					'@test/unstable': '^0.9.0',
				},
			},
			// No changesets - should get auto-changeset from unstable's major bump
		},

		// app_using_stable: Depends on stable (1.5.2 → 2.0.0)
		{
			repo_name: 'app_using_stable',
			repo_url: 'https://gitops.fuz.dev/test/app_using_stable',
			package_json: {
				name: '@test/app_using_stable',
				version: '2.3.0',
				peerDependencies: {
					'@test/stable': '^1.0.0',
				},
			},
			// No changesets - should get auto-changeset from stable's major bump
		},

		// complex_app: Depends on both unstable and stable
		{
			repo_name: 'complex_app',
			repo_url: 'https://gitops.fuz.dev/test/complex_app',
			package_json: {
				name: '@test/complex_app',
				version: '3.0.0',
				dependencies: {
					'@test/unstable': '^0.9.0',
					'@test/stable': '^1.5.0',
				},
			},
			changesets: [
				{
					filename: 'patch.md',
					content: `---
"@test/complex_app": patch
---

Small fix (will escalate to major due to dependencies)`,
				},
			],
		},
	],

	expected_outcomes: {
		publishing_order: [
			'@test/stable',
			'@test/unstable',
			'@test/complex_app',
			'@test/app_using_stable',
			'@test/app_using_unstable',
		],

		version_changes: [
			{
				package_name: '@test/unstable',
				from: '0.9.5',
				to: '1.0.0', // 0.x → 1.0 transition
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/stable',
				from: '1.5.2',
				to: '2.0.0', // 1.x → 2.0 transition
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/app_using_unstable',
				from: '1.0.0',
				to: '2.0.0', // Major bump from unstable's major change
				scenario: 'auto_generated',
			},
			{
				package_name: '@test/app_using_stable',
				from: '2.3.0',
				to: '3.0.0', // Major bump from stable's major change
				scenario: 'auto_generated',
			},
			{
				package_name: '@test/complex_app',
				from: '3.0.0',
				to: '4.0.0', // Correctly escalated from patch to major
				scenario: 'bump_escalation',
			},
		],

		breaking_cascades: {
			'@test/unstable': ['@test/app_using_unstable', '@test/complex_app'],
			'@test/stable': ['@test/app_using_stable', '@test/complex_app'],
		},

		warnings: [],
		errors: [],
	},
};
