import { assert, describe, test } from 'vitest';

import {
	normalize_gitops_config,
	create_empty_gitops_config,
	type GitopsRepoConfig,
	type RawGitopsRepoConfig
} from '$lib/gitops_config.ts';
import type { Url } from '@fuzdev/fuz_util/url.ts';

/** Normalizes a single raw repo entry and returns its parsed config. */
const parse_one = (raw: Url | RawGitopsRepoConfig): GitopsRepoConfig => {
	const { repos } = normalize_gitops_config({ repos: [raw] });
	const [first] = repos;
	assert(first);
	return first;
};

describe('normalize_gitops_config', () => {
	describe('top-level config', () => {
		test('empty config falls back to defaults', () => {
			const empty = create_empty_gitops_config();
			const config = normalize_gitops_config({});
			assert.deepEqual(config.repos, []);
			assert.equal(config.repos_dir, empty.repos_dir);
		});

		test('preserves a provided repos_dir', () => {
			const config = normalize_gitops_config({ repos_dir: '/custom/repos' });
			assert.equal(config.repos_dir, '/custom/repos');
		});

		test('undefined repos normalizes to an empty array', () => {
			assert.deepEqual(normalize_gitops_config({ repos: undefined }).repos, []);
		});
	});

	describe('string repo entries', () => {
		test('applies all defaults', () => {
			const repo = parse_one('https://github.com/fuzdev/fuz_ui');
			assert.deepEqual(repo, {
				repo_url: 'https://github.com/fuzdev/fuz_ui',
				repo_dir: null,
				branch: 'main',
				visibility: 'public',
				ci: true,
				archived: false
			});
		});
	});

	describe('object repo entries', () => {
		test('applies defaults for a minimal entry', () => {
			const repo = parse_one({ repo_url: 'https://github.com/fuzdev/fuz_ui' });
			assert.deepEqual(repo, {
				repo_url: 'https://github.com/fuzdev/fuz_ui',
				repo_dir: null,
				branch: 'main',
				visibility: 'public',
				ci: true,
				archived: false
			});
		});

		test('strips a trailing `.git` from the repo_url', () => {
			const repo = parse_one({ repo_url: 'https://github.com/fuzdev/fuz_ui.git' });
			assert.equal(repo.repo_url, 'https://github.com/fuzdev/fuz_ui');
		});

		test('preserves repo_dir and branch', () => {
			const repo = parse_one({
				repo_url: 'https://github.com/fuzdev/fuz_ui',
				repo_dir: 'some/dir',
				branch: 'next'
			});
			assert.equal(repo.repo_dir, 'some/dir');
			assert.equal(repo.branch, 'next');
		});

		test('null repo_dir is preserved', () => {
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/fuz_ui', repo_dir: null }).repo_dir,
				null
			);
		});
	});

	describe('visibility', () => {
		test('defaults to public', () => {
			assert.equal(parse_one({ repo_url: 'https://github.com/fuzdev/x' }).visibility, 'public');
		});

		test('preserves an explicit private visibility', () => {
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/x', visibility: 'private' }).visibility,
				'private'
			);
		});
	});

	describe('ci derivation', () => {
		test('public repos default ci to true', () => {
			assert.equal(parse_one({ repo_url: 'https://github.com/fuzdev/x' }).ci, true);
		});

		test('private repos default ci to false', () => {
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/x', visibility: 'private' }).ci,
				false
			);
		});

		test('explicit ci overrides the visibility-derived default', () => {
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/x', visibility: 'private', ci: true }).ci,
				true
			);
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/x', visibility: 'public', ci: false }).ci,
				false
			);
		});
	});

	describe('archived', () => {
		test('defaults to false when not provided', () => {
			assert.equal(parse_one({ repo_url: 'https://github.com/fuzdev/x' }).archived, false);
		});

		test('preserves an explicit archived: true', () => {
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/x', archived: true }).archived,
				true
			);
		});

		test('preserves an explicit archived: false', () => {
			assert.equal(
				parse_one({ repo_url: 'https://github.com/fuzdev/x', archived: false }).archived,
				false
			);
		});
	});

	test('normalizes multiple repos preserving order', () => {
		const { repos } = normalize_gitops_config({
			repos: [
				'https://github.com/fuzdev/a',
				{ repo_url: 'https://github.com/fuzdev/b', visibility: 'private' }
			]
		});
		assert.equal(repos.length, 2);
		assert.equal(repos[0]?.repo_url, 'https://github.com/fuzdev/a');
		assert.equal(repos[1]?.repo_url, 'https://github.com/fuzdev/b');
		assert.equal(repos[1]?.visibility, 'private');
	});
});
