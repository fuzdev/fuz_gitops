import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests circular devDependencies between packages.
 * Real-world scenario: testing tools or packages that test each other.
 *
 * Structure:
 * - tool_a ←→ tool_b (circular devDependencies)
 * - consumer depends on both (production deps)
 *
 * Should publish successfully because topological sort excludes dev dependencies.
 */
export const circular_dev_deps: RepoFixtureSet = {
	name: 'circular_dev_deps',
	description:
		'Tests that circular devDependencies do not prevent publishing (dev deps excluded from topological sort)',

	repos: [
		// tool_a: Has explicit minor changeset, devDep on tool_b
		{
			repo_name: 'tool_a',
			repo_url: 'https://gitops.fuz.dev/test/tool_a',
			package_json: {
				name: '@test/tool_a',
				version: '1.0.0',
				devDependencies: {
					'@test/tool_b': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'feature.md',
					content: `---
"@test/tool_a": minor
---

New feature in tool_a`,
				},
			],
		},

		// tool_b: Has explicit patch changeset, devDep on tool_a (creates cycle)
		{
			repo_name: 'tool_b',
			repo_url: 'https://gitops.fuz.dev/test/tool_b',
			package_json: {
				name: '@test/tool_b',
				version: '1.0.0',
				devDependencies: {
					'@test/tool_a': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'fix.md',
					content: `---
"@test/tool_b": patch
---

Bug fix in tool_b`,
				},
			],
		},

		// consumer: Depends on both tools via production dependencies
		{
			repo_name: 'consumer',
			repo_url: 'https://gitops.fuz.dev/test/consumer',
			package_json: {
				name: '@test/consumer',
				version: '1.0.0',
				dependencies: {
					'@test/tool_a': '^1.0.0',
					'@test/tool_b': '^1.0.0',
				},
			},
			// No changesets, but should get auto-changeset due to dependency updates
		},
	],

	expected_outcomes: {
		// Publishing order: tool_a and tool_b can be in either order (no prod dep cycle),
		// then consumer depends on both
		// Note: Order between tool_a and tool_b is non-deterministic (both valid)
		publishing_order: ['@test/tool_b', '@test/tool_a', '@test/consumer'],

		version_changes: [
			{
				package_name: '@test/tool_a',
				from: '1.0.0',
				to: '1.1.0', // minor bump
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/tool_b',
				from: '1.0.0',
				to: '1.0.1', // patch bump
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/consumer',
				from: '1.0.0',
				to: '1.0.1', // Patch bump: tool_a's minor (1.0.0 → 1.1.0) is NOT breaking in >=1.0
				scenario: 'auto_generated',
			},
		],

		// No breaking cascades: tool_a's minor bump is NOT breaking in >=1.0 (only major is)
		breaking_cascades: {},

		info: ['1 dev dependency cycle(s) detected (normal, shown in gitops_analyze)'],
		warnings: [],
		errors: [],
	},
};
