import {assert, describe, test} from 'vitest';
import {join} from 'node:path';
import {mkdtemp, writeFile, mkdir, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';

import {
	should_exclude_path,
	walk_repo_files,
	collect_repo_files,
	DEFAULT_EXCLUDE_DIRS,
	DEFAULT_EXCLUDE_EXTENSIONS,
} from '$lib/repo_ops.js';

describe('repo_ops', () => {
	describe('DEFAULT_EXCLUDE_DIRS', () => {
		test('includes common directories to skip', () => {
			assert.ok(DEFAULT_EXCLUDE_DIRS.includes('node_modules'));
			assert.ok(DEFAULT_EXCLUDE_DIRS.includes('.git'));
			assert.ok(DEFAULT_EXCLUDE_DIRS.includes('.svelte-kit'));
			assert.ok(DEFAULT_EXCLUDE_DIRS.includes('dist'));
		});
	});

	describe('DEFAULT_EXCLUDE_EXTENSIONS', () => {
		test('includes binary file extensions', () => {
			assert.ok(DEFAULT_EXCLUDE_EXTENSIONS.includes('.png'));
			assert.ok(DEFAULT_EXCLUDE_EXTENSIONS.includes('.jpg'));
			assert.ok(DEFAULT_EXCLUDE_EXTENSIONS.includes('.woff2'));
		});

		test('includes lock files', () => {
			assert.ok(DEFAULT_EXCLUDE_EXTENSIONS.includes('.lock'));
		});
	});

	describe('should_exclude_path', () => {
		test('excludes paths containing default excluded directories', () => {
			assert.strictEqual(should_exclude_path('/project/node_modules/foo.js'), true);
			assert.strictEqual(should_exclude_path('/project/.git/config'), true);
			assert.strictEqual(should_exclude_path('/project/.svelte-kit/output/foo.js'), true);
			assert.strictEqual(should_exclude_path('/project/dist/bundle.js'), true);
		});

		test('excludes paths with default excluded extensions', () => {
			assert.strictEqual(should_exclude_path('/project/image.png'), true);
			assert.strictEqual(should_exclude_path('/project/font.woff2'), true);
			assert.strictEqual(should_exclude_path('/project/package-lock.lock'), true);
		});

		test('does not exclude normal source files', () => {
			assert.strictEqual(should_exclude_path('/project/src/lib/foo.ts'), false);
			assert.strictEqual(should_exclude_path('/project/src/routes/+page.svelte'), false);
			assert.strictEqual(should_exclude_path('/project/README.md'), false);
		});

		test('respects custom exclude_dirs option', () => {
			const options = {exclude_dirs: ['custom_dir']};
			assert.strictEqual(should_exclude_path('/project/custom_dir/foo.ts', options), true);
			// Default dirs are still excluded
			assert.strictEqual(should_exclude_path('/project/node_modules/foo.ts', options), true);
		});

		test('respects custom exclude_extensions option', () => {
			const options = {exclude_extensions: ['.custom']};
			assert.strictEqual(should_exclude_path('/project/file.custom', options), true);
			// Default extensions are still excluded
			assert.strictEqual(should_exclude_path('/project/image.png', options), true);
		});

		test('respects no_defaults option', () => {
			const options = {no_defaults: true, exclude_dirs: ['only_this']};
			// Default dirs no longer excluded
			assert.strictEqual(should_exclude_path('/project/node_modules/foo.ts', options), false);
			// Custom dir is excluded
			assert.strictEqual(should_exclude_path('/project/only_this/foo.ts', options), true);
		});
	});

	describe('walk_repo_files', () => {
		let temp_dir: string;

		// Create a temporary directory structure for testing
		const setup_temp_dir = async (): Promise<string> => {
			const dir = await mkdtemp(join(tmpdir(), 'repo_ops_test_'));

			// Create file structure
			await mkdir(join(dir, 'src', 'lib'), {recursive: true});
			await mkdir(join(dir, 'node_modules', 'pkg'), {recursive: true});
			await mkdir(join(dir, '.git'), {recursive: true});

			await writeFile(join(dir, 'src', 'lib', 'foo.ts'), 'export const foo = 1;');
			await writeFile(join(dir, 'src', 'lib', 'bar.ts'), 'export const bar = 2;');
			await writeFile(join(dir, 'src', 'index.ts'), 'export * from "./lib/foo.js";');
			await writeFile(join(dir, 'node_modules', 'pkg', 'index.js'), 'module.exports = {};');
			await writeFile(join(dir, '.git', 'config'), '[core]');
			await writeFile(join(dir, 'image.png'), 'binary data');
			await writeFile(join(dir, 'README.md'), '# Test');

			return dir;
		};

		const cleanup_temp_dir = async (dir: string): Promise<void> => {
			await rm(dir, {recursive: true, force: true});
		};

		test('walks files excluding default directories', async () => {
			temp_dir = await setup_temp_dir();
			try {
				const files = await collect_repo_files(temp_dir);

				// Should include source files
				assert.ok(files.some((f) => f.endsWith('foo.ts')));
				assert.ok(files.some((f) => f.endsWith('bar.ts')));
				assert.ok(files.some((f) => f.endsWith('index.ts')));
				assert.ok(files.some((f) => f.endsWith('README.md')));

				// Should exclude node_modules and .git
				assert.ok(!files.some((f) => f.includes('node_modules')));
				assert.ok(!files.some((f) => f.includes('.git')));

				// Should exclude binary files
				assert.ok(!files.some((f) => f.endsWith('.png')));
			} finally {
				await cleanup_temp_dir(temp_dir);
			}
		});

		test('yields files via async generator', async () => {
			temp_dir = await setup_temp_dir();
			try {
				const files: Array<string> = [];
				for await (const file of walk_repo_files(temp_dir)) {
					files.push(file);
				}

				assert.ok(files.length > 0);
				assert.ok(files.some((f) => f.endsWith('.ts')));
			} finally {
				await cleanup_temp_dir(temp_dir);
			}
		});

		test('includes directories when include_dirs is true', async () => {
			temp_dir = await setup_temp_dir();
			try {
				const files = await collect_repo_files(temp_dir, {include_dirs: true});

				// Should include the src and src/lib directories
				assert.ok(files.some((f) => f.endsWith('/src')));
				assert.ok(files.some((f) => f.endsWith('/src/lib')));
			} finally {
				await cleanup_temp_dir(temp_dir);
			}
		});

		test('respects custom exclusions', async () => {
			temp_dir = await setup_temp_dir();
			try {
				// Exclude .md files
				const files = await collect_repo_files(temp_dir, {
					exclude_extensions: ['.md'],
				});

				assert.ok(!files.some((f) => f.endsWith('README.md')));
				assert.ok(files.some((f) => f.endsWith('.ts')));
			} finally {
				await cleanup_temp_dir(temp_dir);
			}
		});

		test('handles non-existent directories gracefully', async () => {
			const files = await collect_repo_files('/non/existent/path');
			assert.deepEqual(files, []);
		});
	});

	describe('collect_repo_files', () => {
		test('returns array of all walked files', async () => {
			const temp_dir = await mkdtemp(join(tmpdir(), 'repo_ops_collect_'));
			try {
				await writeFile(join(temp_dir, 'a.ts'), 'a');
				await writeFile(join(temp_dir, 'b.ts'), 'b');

				const files = await collect_repo_files(temp_dir);

				assert.strictEqual(Array.isArray(files), true);
				assert.strictEqual(files.length, 2);
				assert.ok(files.some((f) => f.endsWith('a.ts')));
				assert.ok(files.some((f) => f.endsWith('b.ts')));
			} finally {
				await rm(temp_dir, {recursive: true, force: true});
			}
		});
	});
});
