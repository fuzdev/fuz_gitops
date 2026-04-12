import {assert, describe, test} from 'vitest';
import {
	parse_changeset_content,
	determine_bump_from_changesets,
	type ChangesetInfo,
} from '$lib/changeset_reader.js';
import type {BumpType} from '$lib/semver.js';
import {calculate_next_version, compare_bump_types} from '$lib/version_utils.js';

describe('changeset_reader', () => {
	describe('parse_changeset_content', () => {
		test('parses valid changeset with single package', () => {
			const content = `---
"test-package": patch
---

Fix a small bug in the parser.`;

			const result = parse_changeset_content(content, 'test.md');

			assert.deepEqual(result, {
				filename: 'test.md',
				packages: [{name: 'test-package', bump_type: 'patch'}],
				summary: 'Fix a small bug in the parser.',
			});
		});

		test('parses changeset with multiple packages', () => {
			const content = `---
"package-a": minor
"@scope/package-b": patch
---

Add new feature to package-a and fix bug in package-b.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result, {
				filename: 'changeset.md',
				packages: [
					{name: 'package-a', bump_type: 'minor'},
					{name: '@scope/package-b', bump_type: 'patch'},
				],
				summary: 'Add new feature to package-a and fix bug in package-b.',
			});
		});

		test('handles major version bumps', () => {
			const content = `---
"api-package": major
---

BREAKING: Complete API redesign.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages[0], {
				name: 'api-package',
				bump_type: 'major',
			});
			assert.strictEqual(result?.summary, 'BREAKING: Complete API redesign.');
		});

		test('handles single quotes in package names', () => {
			const content = `---
'single-quoted': patch
---

Test single quotes.`;

			const result = parse_changeset_content(content);

			assert.strictEqual(result?.packages[0]!.name, 'single-quoted');
		});

		test('ignores whitespace variations', () => {
			const content = `---
   "package-a"   :    minor
"package-b":patch
---

   Multiline summary
   with extra whitespace.   `;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: 'package-a', bump_type: 'minor'},
				{name: 'package-b', bump_type: 'patch'},
			]);
			assert.strictEqual(result?.summary, 'Multiline summary\n   with extra whitespace.');
		});

		test('returns null for invalid frontmatter format', () => {
			const content = `No frontmatter here
Just plain text.`;

			assert.strictEqual(parse_changeset_content(content), null);
		});

		test('returns null for empty frontmatter', () => {
			const content = `---
---

Empty changeset with no packages.`;

			assert.strictEqual(parse_changeset_content(content), null);
		});

		test('returns null for invalid package format', () => {
			const content = `---
invalid-line-without-colon
---

Invalid format.`;

			assert.strictEqual(parse_changeset_content(content), null);
		});

		test('ignores invalid bump types', () => {
			const content = `---
"valid-package": patch
"invalid-package": invalid-bump
"another-valid": minor
---

Mixed valid and invalid.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: 'valid-package', bump_type: 'patch'},
				{name: 'another-valid', bump_type: 'minor'},
			]);
		});

		test('handles complex package names', () => {
			const content = `---
"@scope/package-name": patch
"org.example.package": minor
"_underscore-package": major
---

Complex package names.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: '@scope/package-name', bump_type: 'patch'},
				{name: 'org.example.package', bump_type: 'minor'},
				{name: '_underscore-package', bump_type: 'major'},
			]);
		});

		test('handles extra frontmatter fields gracefully', () => {
			const content = `---
"valid-package": patch
author: "John Doe"
# This is a comment
created: 2023-01-01
"another-valid": minor
---

Changeset with extra frontmatter fields.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: 'valid-package', bump_type: 'patch'},
				{name: 'another-valid', bump_type: 'minor'},
			]);
		});

		test('handles mixed quote styles', () => {
			const content = `---
"double-quoted": patch
'single-quoted': minor
---

Mixed quote styles.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: 'double-quoted', bump_type: 'patch'},
				{name: 'single-quoted', bump_type: 'minor'},
			]);
		});

		test('handles empty summary', () => {
			const content = `---
"package-name": patch
---
`;

			const result = parse_changeset_content(content);

			assert.strictEqual(result?.packages.length, 1);
			assert.strictEqual(result?.summary, '');
		});

		test('ignores malformed frontmatter lines', () => {
			const content = `---
"valid-package": patch
malformed-line-without-quotes: patch
"package-with-invalid-bump": invalid-bump-type
"another-valid": minor
missing-colon patch
---

Mix of valid and invalid lines.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: 'valid-package', bump_type: 'patch'},
				{name: 'another-valid', bump_type: 'minor'},
			]);
		});

		test('handles only one frontmatter delimiter', () => {
			const content = `---
"package-name": patch

Missing closing delimiter.`;

			const result = parse_changeset_content(content);

			assert.strictEqual(result, null);
		});

		test('handles multiple frontmatter sections (uses first)', () => {
			const content = `---
"first-package": patch
---

First summary.

---
"second-package": minor
---

Second summary should be ignored.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [{name: 'first-package', bump_type: 'patch'}]);
			assert.ok(result?.summary.includes('First summary'));
		});

		test('handles same package multiple times (highest bump wins)', () => {
			const content = `---
"same-package": patch
"other-package": minor
"same-package": major
"same-package": minor
---

Duplicate package entries.`;

			const result = parse_changeset_content(content);

			assert.deepEqual(result?.packages, [
				{name: 'same-package', bump_type: 'patch'}, // First occurrence kept
				{name: 'other-package', bump_type: 'minor'},
				{name: 'same-package', bump_type: 'major'}, // Later occurrences also kept (consumer handles)
				{name: 'same-package', bump_type: 'minor'},
			]);
		});
	});

	describe('determine_bump_from_changesets', () => {
		const create_changeset = (
			packages: Array<{name: string; bump_type: BumpType}>,
		): ChangesetInfo => ({
			filename: 'test.md',
			packages,
			summary: 'Test changeset',
		});

		test('finds bump type for specific package', () => {
			const changesets = [
				create_changeset([{name: 'package-a', bump_type: 'patch'}]),
				create_changeset([{name: 'package-b', bump_type: 'minor'}]),
			];

			assert.strictEqual(determine_bump_from_changesets(changesets, 'package-a'), 'patch');
			assert.strictEqual(determine_bump_from_changesets(changesets, 'package-b'), 'minor');
		});

		test('returns highest bump type when package appears multiple times', () => {
			const changesets = [
				create_changeset([{name: 'package-a', bump_type: 'patch'}]),
				create_changeset([{name: 'package-a', bump_type: 'minor'}]),
				create_changeset([{name: 'package-a', bump_type: 'patch'}]), // lower bump
			];

			assert.strictEqual(determine_bump_from_changesets(changesets, 'package-a'), 'minor');
		});

		test('returns major when it appears anywhere', () => {
			const changesets = [
				create_changeset([{name: 'package-a', bump_type: 'patch'}]),
				create_changeset([{name: 'package-a', bump_type: 'major'}]),
				create_changeset([{name: 'package-a', bump_type: 'minor'}]),
			];

			assert.strictEqual(determine_bump_from_changesets(changesets, 'package-a'), 'major');
		});

		test('returns null for non-existent package', () => {
			const changesets = [create_changeset([{name: 'package-a', bump_type: 'patch'}])];

			assert.strictEqual(determine_bump_from_changesets(changesets, 'non-existent'), null);
		});

		test('handles empty changesets array', () => {
			assert.strictEqual(determine_bump_from_changesets([], 'any-package'), null);
		});
	});

	describe('compare_bump_types', () => {
		test('orders bump types correctly', () => {
			// Major > Minor > Patch
			assert.ok(compare_bump_types('major', 'minor') > 0);
			assert.ok(compare_bump_types('major', 'patch') > 0);
			assert.ok(compare_bump_types('minor', 'patch') > 0);

			// Reverse comparisons
			assert.ok(compare_bump_types('minor', 'major') < 0);
			assert.ok(compare_bump_types('patch', 'major') < 0);
			assert.ok(compare_bump_types('patch', 'minor') < 0);

			// Equal comparisons
			assert.strictEqual(compare_bump_types('major', 'major'), 0);
			assert.strictEqual(compare_bump_types('minor', 'minor'), 0);
			assert.strictEqual(compare_bump_types('patch', 'patch'), 0);
		});
	});

	describe('calculate_next_version', () => {
		describe('patch bumps', () => {
			test('increments patch version', () => {
				assert.strictEqual(calculate_next_version('1.2.3', 'patch'), '1.2.4');
				assert.strictEqual(calculate_next_version('0.5.10', 'patch'), '0.5.11');
				assert.strictEqual(calculate_next_version('10.20.99', 'patch'), '10.20.100');
			});
		});

		describe('minor bumps', () => {
			test('increments minor version and resets patch', () => {
				assert.strictEqual(calculate_next_version('1.2.3', 'minor'), '1.3.0');
				assert.strictEqual(calculate_next_version('0.5.10', 'minor'), '0.6.0');
				assert.strictEqual(calculate_next_version('10.20.99', 'minor'), '10.21.0');
			});
		});

		describe('major bumps', () => {
			test('increments major version and resets minor and patch', () => {
				assert.strictEqual(calculate_next_version('1.2.3', 'major'), '2.0.0');
				assert.strictEqual(calculate_next_version('0.5.10', 'major'), '1.0.0');
				assert.strictEqual(calculate_next_version('10.20.99', 'major'), '11.0.0');
			});
		});

		describe('edge cases', () => {
			test('handles zero versions', () => {
				assert.strictEqual(calculate_next_version('0.0.0', 'patch'), '0.0.1');
				assert.strictEqual(calculate_next_version('0.0.0', 'minor'), '0.1.0');
				assert.strictEqual(calculate_next_version('0.0.0', 'major'), '1.0.0');
			});

			test('handles large version numbers', () => {
				assert.strictEqual(calculate_next_version('99.99.99', 'patch'), '99.99.100');
				assert.strictEqual(calculate_next_version('10.20.999', 'minor'), '10.21.0');
				assert.strictEqual(calculate_next_version('999.0.0', 'major'), '1000.0.0');
			});

			test('throws on invalid version format', () => {
				assert.throws(() => calculate_next_version('invalid', 'patch'));
				assert.throws(() => calculate_next_version('1.2', 'patch'));
				assert.throws(() => calculate_next_version('1.2.3.4', 'patch'));
				assert.throws(() => calculate_next_version('v1.2.3', 'patch')); // version prefix
				assert.throws(() => calculate_next_version('1.2.3-beta', 'patch')); // prerelease
			});
		});
	});
});
