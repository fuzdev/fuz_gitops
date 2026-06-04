import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests handling of private packages (private: true in package.json).
 * Private packages are excluded from version changes — they never publish, escalate, or
 * get an auto-changeset — while their dependents still publish normally. A private package
 * keeps its place in the topological order (publishing_order) but produces no version change.
 *
 * Structure:
 * - public_lib: Public package with explicit changeset
 * - private_tool: Private package with a changeset that will NOT be published (warns)
 * - consumer: Depends on public_lib (prod) and private_tool (dev)
 */
export const private_packages: RepoFixtureSet = {
	name: 'private_packages',
	description:
		'Tests that private packages are excluded from publishing but dependents can publish',

	repos: [
		// public_lib: Normal public package with breaking change
		{
			repo_name: 'public_lib',
			repo_url: 'https://gitops.fuz.dev/test/public_lib',
			package_json: {
				name: '@test/public_lib',
				version: '1.0.0',
			},
			changesets: [
				{
					filename: 'feature.md',
					content: `---
"@test/public_lib": minor
---

New feature in public_lib`,
				},
			],
		},

		// private_tool: Private package (should be skipped entirely)
		{
			repo_name: 'private_tool',
			repo_url: 'https://gitops.fuz.dev/test/private_tool',
			package_json: {
				name: '@test/private_tool',
				version: '1.0.0',
				private: true,
			},
			changesets: [
				{
					filename: 'update.md',
					content: `---
"@test/private_tool": minor
---

Update to private_tool (should not publish)`,
				},
			],
		},

		// consumer: Depends on both public and private packages
		{
			repo_name: 'consumer',
			repo_url: 'https://gitops.fuz.dev/test/consumer',
			package_json: {
				name: '@test/consumer',
				version: '1.0.0',
				dependencies: {
					'@test/public_lib': '^1.0.0',
				},
				devDependencies: {
					'@test/private_tool': '^1.0.0', // Private pkg as dev dependency
				},
			},
			// No changesets, but should get auto-changeset from public_lib update
		},
	],

	expected_outcomes: {
		// private_tool keeps its topological slot but never publishes (no version change)
		publishing_order: ['@test/public_lib', '@test/consumer', '@test/private_tool'],

		version_changes: [
			{
				package_name: '@test/public_lib',
				from: '1.0.0',
				to: '1.1.0',
				scenario: 'explicit_changeset',
			},
			// private_tool is intentionally absent: a private package is excluded from version
			// changes (it never publishes), even though it has a changeset.
			{
				package_name: '@test/consumer',
				from: '1.0.0',
				to: '1.0.1', // Patch bump: public_lib's minor (1.0.0 → 1.1.0) is NOT breaking in >=1.0
				scenario: 'auto_generated',
			},
		],

		// No breaking cascades: public_lib's minor bump is NOT breaking in >=1.0 (only major is)
		breaking_cascades: {},

		// A private package carrying a changeset is flagged, since that changeset can't publish.
		warnings: ['@test/private_tool is private — its changeset(s) will not be published'],
		errors: [],
	},
};
