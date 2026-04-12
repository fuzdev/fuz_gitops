import {assert, describe, test} from 'vitest';
import {
	generate_changeset_content,
	create_dependency_updates,
	type DependencyVersionChange,
} from '$lib/changeset_generator.js';
import type {PublishedVersion} from '$lib/multi_repo_publisher.js';

describe('changeset_generator', () => {
	describe('generate_changeset_content', () => {
		test('generates content for patch updates', () => {
			const updates: Array<DependencyVersionChange> = [
				{
					package_name: 'lib-a',
					from_version: '1.0.0',
					to_version: '1.0.1',
					bump_type: 'patch',
					breaking: false,
				},
				{
					package_name: 'lib-b',
					from_version: '2.1.0',
					to_version: '2.1.5',
					bump_type: 'patch',
					breaking: false,
				},
			];

			const content = generate_changeset_content('my-package', updates, 'patch');

			assert.ok(content.includes('"my-package": patch'));
			assert.ok(content.includes('Update dependencies'));
			assert.ok(content.includes('Updated dependencies:'));
			assert.ok(content.includes('- lib-a: 1.0.0 → 1.0.1 (patch)'));
			assert.ok(content.includes('- lib-b: 2.1.0 → 2.1.5 (patch)'));
			assert.ok(!content.includes('BREAKING'));
		});

		test('generates content for breaking changes', () => {
			const updates: Array<DependencyVersionChange> = [
				{
					package_name: 'lib-breaking',
					from_version: '0.5.0',
					to_version: '0.6.0',
					bump_type: 'minor',
					breaking: true,
				},
			];

			const content = generate_changeset_content('my-package', updates, 'minor');

			assert.ok(content.includes('"my-package": minor'));
			assert.ok(content.includes('Update dependencies (BREAKING CHANGES)'));
			assert.ok(content.includes('Breaking dependency changes:'));
			assert.ok(content.includes('- lib-breaking: 0.5.0 → 0.6.0 (minor)'));
		});

		test('generates content for mixed breaking and regular updates', () => {
			const updates: Array<DependencyVersionChange> = [
				{
					package_name: 'breaking-lib',
					from_version: '1.0.0',
					to_version: '2.0.0',
					bump_type: 'major',
					breaking: true,
				},
				{
					package_name: 'regular-lib',
					from_version: '1.0.0',
					to_version: '1.0.1',
					bump_type: 'patch',
					breaking: false,
				},
			];

			const content = generate_changeset_content('my-package', updates, 'major');

			assert.ok(content.includes('"my-package": major'));
			assert.ok(content.includes('Update dependencies (BREAKING CHANGES)'));
			assert.ok(content.includes('Breaking dependency changes:'));
			assert.ok(content.includes('- breaking-lib: 1.0.0 → 2.0.0 (major)'));
			assert.ok(content.includes('Other dependency updates:'));
			assert.ok(content.includes('- regular-lib: 1.0.0 → 1.0.1 (patch)'));
		});

		test('handles empty updates array', () => {
			const content = generate_changeset_content('my-package', [], 'patch');

			assert.ok(content.includes('"my-package": patch'));
			assert.ok(content.includes('Update dependencies'));
			assert.ok(!content.includes('Updated dependencies:'));
			assert.ok(!content.includes('Breaking dependency changes:'));
		});

		test('generates valid changeset format', () => {
			const updates: Array<DependencyVersionChange> = [
				{
					package_name: 'lib',
					from_version: '1.0.0',
					to_version: '1.1.0',
					bump_type: 'minor',
					breaking: false,
				},
			];

			const content = generate_changeset_content('test-pkg', updates, 'minor');

			// Should start with frontmatter
			assert.match(content, /^---\n/);
			// Should have package declaration
			assert.ok(content.includes('"test-pkg": minor'));
			// Should close frontmatter
			assert.match(content, /\n---\n/);
			// Should have summary after frontmatter
			assert.match(content, /---\n\nUpdate dependencies/);
		});

		test('escapes package names in frontmatter', () => {
			const updates: Array<DependencyVersionChange> = [];

			const content = generate_changeset_content('@scope/package-name', updates, 'patch');

			assert.ok(content.includes('"@scope/package-name": patch'));
		});
	});

	describe('create_dependency_updates', () => {
		test('creates updates from published versions', () => {
			const dependencies = new Map([
				['lib-a', '^1.0.0'],
				['lib-b', '~2.0.0'],
				['external-lib', '^3.0.0'], // not published
			]);

			const published_versions: Map<string, PublishedVersion> = new Map([
				[
					'lib-a',
					{
						name: 'lib-a',
						old_version: '1.0.0',
						new_version: '1.1.0',
						bump_type: 'minor',
						breaking: false,
						commit: 'abc123',
						tag: 'v1.1.0',
					},
				],
				[
					'lib-b',
					{
						name: 'lib-b',
						old_version: '2.0.0',
						new_version: '2.0.1',
						bump_type: 'patch',
						breaking: false,
						commit: 'def456',
						tag: 'v2.0.1',
					},
				],
			]);

			const updates = create_dependency_updates(dependencies, published_versions);

			assert.strictEqual(updates.length, 2);

			const lib_a_update = updates.find((u) => u.package_name === 'lib-a')!;
			assert.deepEqual(lib_a_update, {
				package_name: 'lib-a',
				from_version: '1.0.0', // stripped prefix
				to_version: '1.1.0',
				bump_type: 'minor',
				breaking: false,
			});

			const lib_b_update = updates.find((u) => u.package_name === 'lib-b')!;
			assert.deepEqual(lib_b_update, {
				package_name: 'lib-b',
				from_version: '2.0.0', // stripped prefix
				to_version: '2.0.1',
				bump_type: 'patch',
				breaking: false,
			});

			// Should not include external-lib (not published)
			assert.strictEqual(
				updates.find((u) => u.package_name === 'external-lib'),
				undefined,
			);
		});

		test('handles breaking changes', () => {
			const dependencies = new Map([['breaking-lib', '^0.5.0']]);

			const published_versions: Map<string, PublishedVersion> = new Map([
				[
					'breaking-lib',
					{
						name: 'breaking-lib',
						old_version: '0.5.0',
						new_version: '0.6.0',
						bump_type: 'minor',
						breaking: true, // Pre-1.0 minor is breaking
						commit: 'abc123',
						tag: 'v0.6.0',
					},
				],
			]);

			const updates = create_dependency_updates(dependencies, published_versions);

			assert.strictEqual(updates.length, 1);
			assert.strictEqual(updates[0]!.breaking, true);
		});

		test('strips version prefixes from current versions', () => {
			const dependencies = new Map([
				['caret-lib', '^1.0.0'],
				['tilde-lib', '~1.0.0'],
				['exact-lib', '1.0.0'],
				['gte-lib', '>=1.0.0'],
			]);

			const published_versions: Map<string, PublishedVersion> = new Map([
				[
					'caret-lib',
					{
						name: 'caret-lib',
						old_version: '1.0.0',
						new_version: '1.1.0',
						bump_type: 'minor',
						breaking: false,
						commit: 'abc123',
						tag: 'v1.1.0',
					},
				],
				[
					'tilde-lib',
					{
						name: 'tilde-lib',
						old_version: '1.0.0',
						new_version: '1.0.1',
						bump_type: 'patch',
						breaking: false,
						commit: 'def456',
						tag: 'v1.0.1',
					},
				],
				[
					'exact-lib',
					{
						name: 'exact-lib',
						old_version: '1.0.0',
						new_version: '1.0.1',
						bump_type: 'patch',
						breaking: false,
						commit: 'ghi789',
						tag: 'v1.0.1',
					},
				],
				[
					'gte-lib',
					{
						name: 'gte-lib',
						old_version: '1.0.0',
						new_version: '1.0.1',
						bump_type: 'patch',
						breaking: false,
						commit: 'jkl012',
						tag: 'v1.0.1',
					},
				],
			]);

			const updates = create_dependency_updates(dependencies, published_versions);

			// All should have stripped version prefixes
			assert.strictEqual(
				updates.find((u) => u.package_name === 'caret-lib')?.from_version,
				'1.0.0',
			);
			assert.strictEqual(
				updates.find((u) => u.package_name === 'tilde-lib')?.from_version,
				'1.0.0',
			);
			assert.strictEqual(
				updates.find((u) => u.package_name === 'exact-lib')?.from_version,
				'1.0.0',
			);
			assert.strictEqual(updates.find((u) => u.package_name === 'gte-lib')?.from_version, '1.0.0'); // >= fully stripped
		});

		test('handles empty inputs', () => {
			const empty_deps = new Map();
			const empty_published = new Map();

			assert.deepEqual(create_dependency_updates(empty_deps, empty_published), []);
		});
	});
});
