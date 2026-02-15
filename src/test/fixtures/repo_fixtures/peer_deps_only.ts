import type {RepoFixtureSet} from '../repo_fixture_types.js';

/**
 * Tests packages with only peer dependencies (no production dependencies).
 * Common pattern for plugins, adapters, or packages that extend other libraries.
 *
 * Tests:
 * - Peer dependency updates trigger republishing
 * - Breaking changes in peer deps escalate version bumps
 * - Multiple peer deps with different bump types
 * - Both `^` and `>=` version prefixes work correctly
 */
export const peer_deps_only: RepoFixtureSet = {
	name: 'peer_deps_only',
	description:
		'Tests packages with only peer dependencies (plugins/adapters pattern) and bump escalation',

	repos: [
		// core: The core library that plugins depend on
		{
			repo_name: 'core',
			repo_url: 'https://gitops.fuz.dev/test/core',
			package_json: {
				name: '@test/core',
				version: '2.0.0',
			},
			changesets: [
				{
					filename: 'feature.md',
					content: `---
"@test/core": minor
---

New feature in core (minor, not breaking in >=1.0)`,
				},
			],
		},

		// utils: Utility library (non-breaking patch)
		{
			repo_name: 'utils',
			repo_url: 'https://gitops.fuz.dev/test/utils',
			package_json: {
				name: '@test/utils',
				version: '1.0.0',
			},
			changesets: [
				{
					filename: 'fix.md',
					content: `---
"@test/utils": patch
---

Bug fix in utils`,
				},
			],
		},

		// plugin_a: Plugin with peer dep on core only
		{
			repo_name: 'plugin_a',
			repo_url: 'https://gitops.fuz.dev/test/plugin_a',
			package_json: {
				name: '@test/plugin_a',
				version: '1.0.0',
				peerDependencies: {
					'@test/core': '^2.0.0',
				},
			},
			// No changesets - should get auto-changeset from core breaking update
		},

		// plugin_b: Plugin with peer deps on both core and utils
		{
			repo_name: 'plugin_b',
			repo_url: 'https://gitops.fuz.dev/test/plugin_b',
			package_json: {
				name: '@test/plugin_b',
				version: '1.5.0',
				peerDependencies: {
					'@test/core': '^2.0.0',
					'@test/utils': '^1.0.0',
				},
			},
			changesets: [
				{
					filename: 'patch.md',
					content: `---
"@test/plugin_b": patch
---

Small fix (should escalate to minor due to core breaking change)`,
				},
			],
		},

		// adapter: Adapter with only peer deps, no changesets
		// Uses >= prefix (instead of ^) to test prefix handling for controlled packages
		{
			repo_name: 'adapter',
			repo_url: 'https://gitops.fuz.dev/test/adapter',
			package_json: {
				name: '@test/adapter',
				version: '3.0.0',
				peerDependencies: {
					'@test/utils': '>=1.0.0', // >= prefix for controlled package pattern
				},
			},
			// No changesets - utils has patch, should get auto-changeset (patch)
		},
	],

	expected_outcomes: {
		// Note: plugin_a, plugin_b, and adapter have no dependency relationship,
		// so they're ordered deterministically within their tier
		publishing_order: [
			'@test/utils',
			'@test/core',
			'@test/adapter',
			'@test/plugin_b',
			'@test/plugin_a',
		],

		version_changes: [
			{
				package_name: '@test/core',
				from: '2.0.0',
				to: '2.1.0',
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/utils',
				from: '1.0.0',
				to: '1.0.1',
				scenario: 'explicit_changeset',
			},
			{
				package_name: '@test/plugin_a',
				from: '1.0.0',
				to: '1.0.1', // Patch bump - core's minor is NOT breaking in >=1.0
				scenario: 'auto_generated',
			},
			{
				package_name: '@test/adapter',
				from: '3.0.0',
				to: '3.0.1', // Patch bump from utils' patch
				scenario: 'auto_generated',
			},
			{
				package_name: '@test/plugin_b',
				from: '1.5.0',
				to: '1.5.1', // Patch bump: core's minor (2.0.0 → 2.1.0) is NOT breaking in >=1.0
				scenario: 'explicit_changeset',
			},
		],

		// No breaking cascades - core's minor bump is NOT breaking in >=1.0
		breaking_cascades: {},

		warnings: [],
		errors: [],
	},
};
