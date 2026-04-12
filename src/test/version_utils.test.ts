import {assert, describe, test} from 'vitest';

import {
	is_wildcard,
	strip_version_prefix,
	get_version_prefix,
	normalize_version_for_comparison,
	needs_update,
	get_update_prefix,
	is_breaking_change,
	detect_bump_type,
} from '$lib/version_utils.js';

describe('version_utils', () => {
	describe('is_wildcard', () => {
		test('detects wildcard versions', () => {
			assert.strictEqual(is_wildcard('*'), true);
			assert.strictEqual(is_wildcard('^1.0.0'), false);
			assert.strictEqual(is_wildcard('1.0.0'), false);
		});
	});

	describe('strip_version_prefix', () => {
		test('removes caret prefix', () => {
			assert.strictEqual(strip_version_prefix('^1.2.3'), '1.2.3');
		});

		test('removes tilde prefix', () => {
			assert.strictEqual(strip_version_prefix('~1.2.3'), '1.2.3');
		});

		test('removes comparison prefixes', () => {
			assert.strictEqual(strip_version_prefix('>1.2.3'), '1.2.3');
			assert.strictEqual(strip_version_prefix('<1.2.3'), '1.2.3');
			assert.strictEqual(strip_version_prefix('=1.2.3'), '1.2.3');
		});

		test('removes multi-character comparison prefixes', () => {
			assert.strictEqual(strip_version_prefix('>=1.2.3'), '1.2.3');
			assert.strictEqual(strip_version_prefix('<=1.2.3'), '1.2.3');
		});

		test('leaves exact versions unchanged', () => {
			assert.strictEqual(strip_version_prefix('1.2.3'), '1.2.3');
		});
	});

	describe('get_version_prefix', () => {
		test('extracts caret prefix', () => {
			assert.strictEqual(get_version_prefix('^1.2.3'), '^');
		});

		test('extracts tilde prefix', () => {
			assert.strictEqual(get_version_prefix('~1.2.3'), '~');
		});

		test('extracts single-character comparison prefixes', () => {
			assert.strictEqual(get_version_prefix('>1.2.3'), '>');
			assert.strictEqual(get_version_prefix('<1.2.3'), '<');
			assert.strictEqual(get_version_prefix('=1.2.3'), '=');
		});

		test('extracts multi-character comparison prefixes', () => {
			assert.strictEqual(get_version_prefix('>=1.2.3'), '>=');
			assert.strictEqual(get_version_prefix('<=1.2.3'), '<=');
		});

		test('returns empty string for exact versions', () => {
			assert.strictEqual(get_version_prefix('1.2.3'), '');
		});
	});

	describe('normalize_version_for_comparison', () => {
		test('preserves wildcards', () => {
			assert.strictEqual(normalize_version_for_comparison('*'), '*');
		});

		test('handles >= ranges', () => {
			assert.strictEqual(normalize_version_for_comparison('>=1.2.3'), '1.2.3');
		});

		test('strips other prefixes', () => {
			assert.strictEqual(normalize_version_for_comparison('^1.2.3'), '1.2.3');
			assert.strictEqual(normalize_version_for_comparison('~1.2.3'), '1.2.3');
		});

		test('leaves exact versions unchanged', () => {
			assert.strictEqual(normalize_version_for_comparison('1.2.3'), '1.2.3');
		});
	});

	describe('needs_update', () => {
		test('always updates wildcards', () => {
			assert.strictEqual(needs_update('*', '1.0.0'), true);
			assert.strictEqual(needs_update('*', '2.0.0'), true);
		});

		test('updates when normalized versions differ', () => {
			assert.strictEqual(needs_update('^1.0.0', '1.1.0'), true);
			assert.strictEqual(needs_update('~1.0.0', '1.0.1'), true);
		});

		test('does not update when normalized versions are same', () => {
			assert.strictEqual(needs_update('^1.0.0', '1.0.0'), false);
			assert.strictEqual(needs_update('~1.2.3', '1.2.3'), false);
		});

		test('handles different prefixes with same version', () => {
			assert.strictEqual(needs_update('^1.0.0', '~1.0.0'), false); // normalized to same
		});
	});

	describe('get_update_prefix', () => {
		test('uses caret for wildcard replacements', () => {
			assert.strictEqual(get_update_prefix('*', '^'), '^');
			assert.strictEqual(get_update_prefix('*', '~'), '^'); // always caret for wildcards
			assert.strictEqual(get_update_prefix('*', '>='), '^'); // always caret for wildcards
		});

		test('preserves existing prefix', () => {
			assert.strictEqual(get_update_prefix('^1.0.0'), '^');
			assert.strictEqual(get_update_prefix('~1.0.0'), '~');
		});

		test('preserves >= prefix', () => {
			assert.strictEqual(get_update_prefix('>=1.0.0'), '>=');
			assert.strictEqual(get_update_prefix('>=0.38.0'), '>=');
		});

		test('preserves other comparison prefixes', () => {
			assert.strictEqual(get_update_prefix('<=1.0.0'), '<=');
			assert.strictEqual(get_update_prefix('>1.0.0'), '>');
			assert.strictEqual(get_update_prefix('<1.0.0'), '<');
		});

		test('uses default strategy when no prefix', () => {
			assert.strictEqual(get_update_prefix('1.0.0'), '^'); // default is caret
			assert.strictEqual(get_update_prefix('1.0.0', '~'), '~');
			assert.strictEqual(get_update_prefix('1.0.0', ''), '');
			assert.strictEqual(get_update_prefix('1.0.0', '>='), '>=');
		});
	});

	describe('is_breaking_change', () => {
		describe('pre-1.0 versions (0.x.x)', () => {
			test('treats minor bumps as breaking', () => {
				assert.strictEqual(is_breaking_change('0.1.0', 'minor'), true);
				assert.strictEqual(is_breaking_change('0.5.10', 'minor'), true);
			});

			test('treats major bumps as breaking', () => {
				assert.strictEqual(is_breaking_change('0.1.0', 'major'), true);
				assert.strictEqual(is_breaking_change('0.5.10', 'major'), true);
			});

			test('does not treat patch bumps as breaking', () => {
				assert.strictEqual(is_breaking_change('0.1.0', 'patch'), false);
				assert.strictEqual(is_breaking_change('0.5.10', 'patch'), false);
			});
		});

		describe('1.0+ versions', () => {
			test('treats only major bumps as breaking', () => {
				assert.strictEqual(is_breaking_change('1.0.0', 'major'), true);
				assert.strictEqual(is_breaking_change('2.5.10', 'major'), true);
			});

			test('does not treat minor bumps as breaking', () => {
				assert.strictEqual(is_breaking_change('1.0.0', 'minor'), false);
				assert.strictEqual(is_breaking_change('2.5.10', 'minor'), false);
			});

			test('does not treat patch bumps as breaking', () => {
				assert.strictEqual(is_breaking_change('1.0.0', 'patch'), false);
				assert.strictEqual(is_breaking_change('2.5.10', 'patch'), false);
			});
		});
	});

	describe('detect_bump_type', () => {
		test('detects major bumps', () => {
			assert.strictEqual(detect_bump_type('1.2.3', '2.0.0'), 'major');
			assert.strictEqual(detect_bump_type('0.5.0', '1.0.0'), 'major');
		});

		test('detects minor bumps', () => {
			assert.strictEqual(detect_bump_type('1.2.3', '1.3.0'), 'minor');
			assert.strictEqual(detect_bump_type('0.5.0', '0.6.0'), 'minor');
		});

		test('detects patch bumps', () => {
			assert.strictEqual(detect_bump_type('1.2.3', '1.2.4'), 'patch');
			assert.strictEqual(detect_bump_type('0.5.0', '0.5.1'), 'patch');
		});

		test('handles complex version changes', () => {
			// Major takes precedence even with other changes
			assert.strictEqual(detect_bump_type('1.2.3', '2.5.10'), 'major');
			// Minor takes precedence over patch
			assert.strictEqual(detect_bump_type('1.2.3', '1.5.0'), 'minor');
		});
	});
});
