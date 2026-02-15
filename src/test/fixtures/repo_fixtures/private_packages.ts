import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests handling of private packages (private: true in package.json).
 * Private packages should be excluded from publishing, but their dependents
 * should still be able to publish.
 *
 * Structure:
 * - public_lib: Public package with explicit changeset
 * - private_tool: Private package (should be skipped)
 * - consumer: Depends on both public_lib and private_tool
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
		// Note: private_tool appears in publishing order but won't actually publish
		publishing_order: ['@test/public_lib', '@test/consumer', '@test/private_tool'],

		version_changes: [
			{
				package_name: '@test/public_lib',
				from: '1.0.0',
				to: '1.1.0',
				scenario: 'explicit_changeset',
			},
			// Note: private_tool shows in version_changes but won't actually publish
			{
				package_name: '@test/private_tool',
				from: '1.0.0',
				to: '1.1.0',
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/consumer',
				from: '1.0.0',
				to: '1.0.1', // Patch bump: public_lib's minor (1.0.0 → 1.1.0) is NOT breaking in >=1.0
				scenario: 'auto_generated',
			},
		],

		// No breaking cascades: public_lib's minor bump is NOT breaking in >=1.0 (only major is)
		breaking_cascades: {},

		// Note: Private packages appear in publishing order and version_changes,
		// but the actual publisher will skip them based on the private flag.
		// They don't appear in info because they have changesets.

		warnings: [],
		errors: [],
	},
};
