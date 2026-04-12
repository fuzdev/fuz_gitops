import {test, assert, describe} from 'vitest';

import {create_mock_npm_ops} from './test_helpers.js';

describe('install operation', () => {
	test('returns ok:true on success', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.install({cwd: '/some/path'});
		assert.ok(result.ok);
	});

	test('returns ok:false with error on failure', async () => {
		const mock_ops = create_mock_npm_ops({
			install: async () => ({ok: false, message: 'Network error'}),
		});
		const result = await mock_ops.install({cwd: '/some/path'});
		assert.ok(!result.ok);
	});

	test('accepts optional cwd parameter', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.install();
		assert.ok(result.ok);
	});

	test('can override install behavior in mock', async () => {
		let install_called = false;
		let install_cwd: string | undefined;

		const mock_ops = create_mock_npm_ops({
			install: async (options) => {
				install_called = true;
				install_cwd = options?.cwd;
				return {ok: true};
			},
		});

		await mock_ops.install({cwd: '/test/directory'});

		assert.ok(install_called, 'install should have been called');
		assert.equal(install_cwd, '/test/directory');
	});
});

describe('check_package_available operation', () => {
	test('returns true when package exists', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.check_package_available({pkg: 'foo', version: '1.0.0'});
		assert.ok(result.ok);
	});

	test('returns false when package does not exist', async () => {
		const mock_ops = create_mock_npm_ops({
			check_package_available: async () => ({ok: true, value: false}),
		});
		const result = await mock_ops.check_package_available({pkg: 'foo', version: '1.0.0'});
		assert.ok(result.ok);
	});

	test('can override behavior in mock', async () => {
		let check_called = false;
		let pkg_name: string | undefined;
		let pkg_version: string | undefined;

		const mock_ops = create_mock_npm_ops({
			check_package_available: async (options) => {
				check_called = true;
				pkg_name = options.pkg;
				pkg_version = options.version;
				return {ok: true, value: true};
			},
		});

		await mock_ops.check_package_available({pkg: '@scope/package', version: '2.3.4'});

		assert.ok(check_called);
		assert.equal(pkg_name, '@scope/package');
		assert.equal(pkg_version, '2.3.4');
	});
});

describe('check_auth operation', () => {
	test('returns ok:true with username on success', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.check_auth();
		assert.ok(result.ok);
	});

	test('returns ok:false with error when not authenticated', async () => {
		const mock_ops = create_mock_npm_ops({
			check_auth: async () => ({ok: false, message: 'Not logged in to npm'}),
		});
		const result = await mock_ops.check_auth();
		assert.ok(!result.ok);
	});

	test('can override behavior in mock', async () => {
		let auth_called = false;

		const mock_ops = create_mock_npm_ops({
			check_auth: async () => {
				auth_called = true;
				return {ok: true, username: 'custom-user'};
			},
		});

		const result = await mock_ops.check_auth();

		assert.ok(auth_called);
		if (result.ok) {
			assert.equal(result.username, 'custom-user');
		}
	});
});

describe('check_registry operation', () => {
	test('returns ok:true when registry is reachable', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.check_registry();
		assert.ok(result.ok);
	});

	test('returns ok:false with error when registry unreachable', async () => {
		const mock_ops = create_mock_npm_ops({
			check_registry: async () => ({ok: false, message: 'Failed to ping npm registry'}),
		});
		const result = await mock_ops.check_registry();
		assert.ok(!result.ok);
	});

	test('can override behavior in mock', async () => {
		let registry_called = false;

		const mock_ops = create_mock_npm_ops({
			check_registry: async () => {
				registry_called = true;
				return {ok: true};
			},
		});

		await mock_ops.check_registry();

		assert.ok(registry_called);
	});
});

describe('wait_for_package operation', () => {
	test('completes successfully when package becomes available', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.wait_for_package({pkg: 'foo', version: '1.0.0'});
		assert.ok(result.ok);
	});

	test('accepts optional wait options', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.wait_for_package({
			pkg: 'foo',
			version: '1.0.0',
			wait_options: {max_attempts: 5},
		});
		assert.ok(result.ok);
	});

	test('accepts optional logger', async () => {
		const mock_ops = create_mock_npm_ops();
		const result = await mock_ops.wait_for_package({pkg: 'foo', version: '1.0.0', log: undefined});
		assert.ok(result.ok);
	});

	test('can override behavior in mock', async () => {
		let wait_called = false;
		let pkg_name: string | undefined;
		let pkg_version: string | undefined;

		const mock_ops = create_mock_npm_ops({
			wait_for_package: async (options) => {
				wait_called = true;
				pkg_name = options.pkg;
				pkg_version = options.version;
				return {ok: true};
			},
		});

		await mock_ops.wait_for_package({pkg: '@my/package', version: '3.2.1'});

		assert.ok(wait_called);
		assert.equal(pkg_name, '@my/package');
		assert.equal(pkg_version, '3.2.1');
	});
});
