import {assert, test, describe} from 'vitest';
import {semver_compare_versions, semver_bump_version} from '$lib/semver.js';

describe('semver_compare_versions', () => {
	describe('basic version comparison', () => {
		test('compares major versions', () => {
			assert.strictEqual(semver_compare_versions('2.0.0', '1.0.0'), 1);
			assert.strictEqual(semver_compare_versions('1.0.0', '2.0.0'), -1);
			assert.strictEqual(semver_compare_versions('1.0.0', '1.0.0'), 0);
		});

		test('compares minor versions', () => {
			assert.strictEqual(semver_compare_versions('1.2.0', '1.1.0'), 1);
			assert.strictEqual(semver_compare_versions('1.1.0', '1.2.0'), -1);
			assert.strictEqual(semver_compare_versions('1.1.0', '1.1.0'), 0);
		});

		test('compares patch versions', () => {
			assert.strictEqual(semver_compare_versions('1.1.2', '1.1.1'), 1);
			assert.strictEqual(semver_compare_versions('1.1.1', '1.1.2'), -1);
			assert.strictEqual(semver_compare_versions('1.1.1', '1.1.1'), 0);
		});
	});

	describe('prerelease comparison', () => {
		test('normal version has higher precedence than prerelease', () => {
			assert.strictEqual(semver_compare_versions('1.0.0', '1.0.0-alpha'), 1);
			assert.strictEqual(semver_compare_versions('1.0.0-alpha', '1.0.0'), -1);
		});

		test('compares prerelease versions per spec example', () => {
			// Example from spec: 1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-alpha.beta < 1.0.0-beta < 1.0.0-beta.2 < 1.0.0-beta.11 < 1.0.0-rc.1 < 1.0.0
			const ordered = [
				'1.0.0-alpha',
				'1.0.0-alpha.1',
				'1.0.0-alpha.beta',
				'1.0.0-beta',
				'1.0.0-beta.2',
				'1.0.0-beta.11',
				'1.0.0-rc.1',
				'1.0.0',
			];

			for (let i = 0; i < ordered.length - 1; i++) {
				assert.strictEqual(semver_compare_versions(ordered[i]!, ordered[i + 1]!), -1);
				assert.strictEqual(semver_compare_versions(ordered[i + 1]!, ordered[i]!), 1);
			}
		});

		test('numeric identifiers have lower precedence than non-numeric', () => {
			// Per spec: numeric identifiers always have lower precedence than non-numeric
			assert.strictEqual(semver_compare_versions('1.0.0-1', '1.0.0-alpha'), -1);
			assert.strictEqual(semver_compare_versions('1.0.0-alpha', '1.0.0-1'), 1);
		});

		test('compares numeric prerelease identifiers numerically', () => {
			assert.strictEqual(semver_compare_versions('1.0.0-beta.2', '1.0.0-beta.11'), -1);
			assert.strictEqual(semver_compare_versions('1.0.0-beta.11', '1.0.0-beta.2'), 1);
		});

		test('larger set of prerelease fields has higher precedence', () => {
			assert.strictEqual(semver_compare_versions('1.0.0-alpha', '1.0.0-alpha.1'), -1);
			assert.strictEqual(semver_compare_versions('1.0.0-alpha.1', '1.0.0-alpha'), 1);
		});
	});

	describe('build metadata', () => {
		test('ignores build metadata in comparison', () => {
			assert.strictEqual(semver_compare_versions('1.0.0+build1', '1.0.0+build2'), 0);
			assert.strictEqual(semver_compare_versions('1.0.0-alpha+build1', '1.0.0-alpha+build2'), 0);
		});
	});
});

describe('semver_bump_version', () => {
	test('bumps major version', () => {
		assert.strictEqual(semver_bump_version('1.2.3', 'major'), '2.0.0');
		assert.strictEqual(semver_bump_version('0.1.2', 'major'), '1.0.0');
	});

	test('bumps minor version', () => {
		assert.strictEqual(semver_bump_version('1.2.3', 'minor'), '1.3.0');
		assert.strictEqual(semver_bump_version('0.1.2', 'minor'), '0.2.0');
	});

	test('bumps patch version', () => {
		assert.strictEqual(semver_bump_version('1.2.3', 'patch'), '1.2.4');
		assert.strictEqual(semver_bump_version('0.1.2', 'patch'), '0.1.3');
	});

	test('removes prerelease and build metadata', () => {
		assert.strictEqual(semver_bump_version('1.2.3-alpha', 'patch'), '1.2.4');
		assert.strictEqual(semver_bump_version('1.2.3+build', 'patch'), '1.2.4');
		assert.strictEqual(semver_bump_version('1.2.3-alpha+build', 'minor'), '1.3.0');
	});
});
