import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Comprehensive fixture covering all 4 publishing scenarios:
 * 1. Explicit changeset (repo_a)
 * 2. Auto-generated changeset for dependency updates (repo_b)
 * 3. Bump escalation when dependencies require higher bump (repo_c)
 * 4. No changes to publish (repo_d, repo_e)
 */
export const basic_publishing: RepoFixtureSet = {
	name: 'basic_publishing',
	description:
		'Tests all 4 publishing scenarios: explicit changeset, bump escalation, auto-generated changeset, and no changes',

	repos: [
		// repo_a: Leaf package with breaking changeset
		{
			repo_name: 'repo_a',
			repo_url: 'https://gitops.fuz.dev/test/repo_a',
			package_json: {
				name: '@test/repo_a',
				version: '0.1.0',
			},
			changesets: [
				{
					filename: 'breaking.md',
					content: `---
"@test/repo_a": minor
---

Breaking change in repo_a`,
				},
			],
		},

		// repo_b: Auto-changeset scenario (depends on repo_a, no explicit changeset)
		{
			repo_name: 'repo_b',
			repo_url: 'https://gitops.fuz.dev/test/repo_b',
			package_json: {
				name: '@test/repo_b',
				version: '0.1.0',
				dependencies: {
					'@test/repo_a': '^0.1.0',
				},
			},
			// No changesets - should get auto-generated due to repo_a dependency update
		},

		// repo_c: Bump escalation scenario (peer dep on repo_b, patch changeset escalates to minor)
		{
			repo_name: 'repo_c',
			repo_url: 'https://gitops.fuz.dev/test/repo_c',
			package_json: {
				name: '@test/repo_c',
				version: '0.1.0',
				peerDependencies: {
					'@test/repo_b': '^0.1.0',
				},
			},
			changesets: [
				{
					filename: 'patch.md',
					content: `---
"@test/repo_c": patch
---

Patch change in repo_c (will escalate to minor due to breaking dependency)`,
				},
			],
		},

		// repo_d: No changes scenario (no deps, no changeset)
		{
			repo_name: 'repo_d',
			repo_url: 'https://gitops.fuz.dev/test/repo_d',
			package_json: {
				name: '@test/repo_d',
				version: '0.1.0',
			},
			// No changesets, no dependencies - should be in "no changes" info section
		},

		// repo_e: Dev-only dependency (doesn't trigger republish)
		{
			repo_name: 'repo_e',
			repo_url: 'https://gitops.fuz.dev/test/repo_e',
			package_json: {
				name: '@test/repo_e',
				version: '0.1.0',
				devDependencies: {
					'@test/repo_a': '^0.1.0',
				},
			},
			// No changeset - dev deps don't trigger republishing
		},
	],

	expected_outcomes: {
		// Topological sort places packages with no dependencies first
		publishing_order: [
			'@test/repo_a',
			'@test/repo_b',
			'@test/repo_e',
			'@test/repo_d',
			'@test/repo_c',
		],

		version_changes: [
			{
				package_name: '@test/repo_a',
				from: '0.1.0',
				to: '0.2.0',
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/repo_b',
				from: '0.1.0',
				to: '0.2.0',
				scenario: 'auto_generated',
			},
			{
				package_name: '@test/repo_c',
				from: '0.1.0',
				to: '0.2.0', // Correctly escalated from patch due to repo_b's breaking update
				scenario: 'bump_escalation',
			},
		],

		breaking_cascades: {
			// Each entry shows DIRECT dependents, not transitive
			'@test/repo_a': ['@test/repo_b'], // repo_b directly depends on repo_a
			'@test/repo_b': ['@test/repo_c'], // repo_c directly depends on repo_b
		},

		// Packages with no changes to publish
		info: ['@test/repo_d', '@test/repo_e'],

		warnings: [],
		errors: [],
	},
};
