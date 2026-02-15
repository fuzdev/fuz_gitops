import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests three-way circular devDependencies between packages.
 * Real-world scenario: testing tools or dev utilities that depend on each other for development.
 *
 * Structure:
 * - tool_x → tool_y → tool_z → tool_x (3-way circular devDependencies)
 * - app depends on all three (production deps)
 *
 * Should publish successfully because topological sort excludes dev dependencies.
 * Validates that longer dev cycles (>2 packages) work in full publishing workflow.
 */
export const three_way_dev_cycle: RepoFixtureSet = {
	name: 'three_way_dev_cycle',
	description:
		'Tests that three-way circular devDependencies do not prevent publishing (longer cycles work)',

	repos: [
		// tool_x: Has explicit patch changeset, devDep on tool_y
		{
			repo_name: 'tool_x',
			repo_url: 'https://gitops.fuz.dev/test/tool_x',
			package_json: {
				name: '@test/tool_x',
				version: '1.0.0',
				devDependencies: {
					'@test/tool_y': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'fix-x.md',
					content: `---
"@test/tool_x": patch
---

Bug fix in tool_x`,
				},
			],
		},

		// tool_y: Has explicit minor changeset, devDep on tool_z
		{
			repo_name: 'tool_y',
			repo_url: 'https://gitops.fuz.dev/test/tool_y',
			package_json: {
				name: '@test/tool_y',
				version: '1.0.0',
				devDependencies: {
					'@test/tool_z': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'feature-y.md',
					content: `---
"@test/tool_y": minor
---

New feature in tool_y`,
				},
			],
		},

		// tool_z: Has explicit patch changeset, devDep on tool_x (completes 3-way cycle)
		{
			repo_name: 'tool_z',
			repo_url: 'https://gitops.fuz.dev/test/tool_z',
			package_json: {
				name: '@test/tool_z',
				version: '1.0.0',
				devDependencies: {
					'@test/tool_x': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'fix-z.md',
					content: `---
"@test/tool_z": patch
---

Bug fix in tool_z`,
				},
			],
		},

		// app: Depends on all three tools via production dependencies
		{
			repo_name: 'app',
			repo_url: 'https://gitops.fuz.dev/test/app',
			package_json: {
				name: '@test/app',
				version: '1.0.0',
				dependencies: {
					'@test/tool_x': '^1.0.0',
					'@test/tool_y': '^1.0.0',
					'@test/tool_z': '^1.0.0',
				},
			},
			// No changesets, but should get auto-changeset due to dependency updates
		},
	],

	expected_outcomes: {
		// Publishing order: tool_x, tool_y, tool_z can be in any order (no prod dep between them),
		// then app depends on all three
		// Note: Order among the three tools is non-deterministic (all valid)
		publishing_order: ['@test/tool_z', '@test/tool_y', '@test/tool_x', '@test/app'],

		version_changes: [
			{
				package_name: '@test/tool_x',
				from: '1.0.0',
				to: '1.0.1', // patch bump
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/tool_y',
				from: '1.0.0',
				to: '1.1.0', // minor bump
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/tool_z',
				from: '1.0.0',
				to: '1.0.1', // patch bump
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/app',
				from: '1.0.0',
				to: '1.0.1', // Patch bump: tool_y's minor (1.0.0 → 1.1.0) is NOT breaking in >=1.0
				scenario: 'auto_generated',
			},
		],

		// No breaking cascades: tool_y's minor bump is NOT breaking in >=1.0 (only major is)
		breaking_cascades: {},

		info: ['1 dev dependency cycle(s) detected (normal, shown in gitops_analyze)'],
		warnings: [],
		errors: [],
	},
};
