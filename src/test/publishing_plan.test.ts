import { assert, test } from 'vitest';

import type { LocalRepo } from '$lib/local_repo.ts';
import { generate_publishing_plan, version_change_kind } from '$lib/publishing_plan.ts';
import { log_publishing_plan } from '$lib/publishing_plan_logging.ts';
import { derive_publish_steps } from '$lib/publish_steps.ts';
import type { ChangesetOperations } from '$lib/operations.ts';
import { parse_changeset_content } from '$lib/changeset_reader.ts';
import { GITOPS_MAX_ITERATIONS_DEFAULT } from '$lib/gitops_constants.ts';
import { create_mock_repo, create_stream_log } from './test_helpers.ts';

test('detects breaking change cascades', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '0.1.0' }),
		create_mock_repo({ name: 'pkg-b', version: '0.2.0', deps: { 'pkg-a': '0.1.0' } }),
		create_mock_repo({ name: 'pkg-c', version: '0.3.0', deps: { 'pkg-b': '0.2.0' } })
	];

	// Mock changeset operations to simulate breaking changes
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'pkg-a'
		}),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'pkg-a') {
				// Simulate a breaking change for pkg-a
				return { ok: true, version: '0.2.0', bump_type: 'minor' as const };
			}
			return null;
		}
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// pkg-a should have a breaking change (0.x.x minor bump)
	assert.strictEqual(
		plan.version_changes.find((vc) => vc.package_name === 'pkg-a')?.breaking,
		true
	);

	// pkg-b should cascade the breaking change
	assert.strictEqual(plan.breaking_cascades.has('pkg-a'), true);
	assert.ok(plan.breaking_cascades.get('pkg-a')!.includes('pkg-b'));
});

test('handles bump escalation', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '0.1.0' }),
		create_mock_repo({ name: 'pkg-b', version: '0.2.0', deps: { 'pkg-a': '0.1.0' } })
	];

	// Mock operations where pkg-a has breaking change and pkg-b has patch
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: true }),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'pkg-a') {
				return { ok: true, version: '0.2.0', bump_type: 'minor' as const }; // breaking
			}
			if (options.repo.library.name === 'pkg-b') {
				return { ok: true, version: '0.2.1', bump_type: 'patch' as const }; // non-breaking
			}
			return null;
		}
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// pkg-b should have bump escalation due to breaking dep
	const pkg_b_change = plan.version_changes.find((vc) => vc.package_name === 'pkg-b');
	assert.strictEqual(pkg_b_change?.needs_bump_escalation, true);
	assert.strictEqual(pkg_b_change?.required_bump, 'minor');
});

test('generates auto-changesets for dependency updates', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '0.1.0' }),
		create_mock_repo({ name: 'pkg-b', version: '0.2.0', deps: { 'pkg-a': '0.1.0' } }),
		create_mock_repo({ name: 'pkg-c', version: '0.3.0', dev_deps: { 'pkg-a': '0.1.0' } }) // devDep only
	];

	// Mock operations where only pkg-a has changesets
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'pkg-a'
		}),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'pkg-a') {
				return { ok: true, version: '0.1.1', bump_type: 'patch' as const };
			}
			return null;
		}
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// pkg-b should get auto-changeset for dependency update
	const pkg_b_change = plan.version_changes.find((vc) => vc.package_name === 'pkg-b');
	assert.strictEqual(pkg_b_change?.will_generate_changeset, true);
	assert.strictEqual(pkg_b_change?.has_changesets, false);

	// pkg-c should not get auto-changeset (dev dependency only)
	const pkg_c_change = plan.version_changes.find((vc) => vc.package_name === 'pkg-c');
	assert.strictEqual(pkg_c_change, undefined);
});

test('handles circular dev dependencies', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '0.1.0', dev_deps: { 'pkg-b': '0.2.0' } }),
		create_mock_repo({ name: 'pkg-b', version: '0.2.0', dev_deps: { 'pkg-a': '0.1.0' } })
	];

	// Mock operations with no changesets
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: false }),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async () => null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// Should have info about dev cycles (not warnings anymore)
	assert.ok(plan.info.some((i) => i.includes('dev dependency cycle(s) detected')));

	// Should still compute publishing order
	assert.strictEqual(plan.publishing_order.length, 2);

	// Should not have errors
	assert.strictEqual(plan.errors.length, 0);
});

test('detects production circular dependencies', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '0.1.0', deps: { 'pkg-b': '0.2.0' } }),
		create_mock_repo({ name: 'pkg-b', version: '0.2.0', deps: { 'pkg-a': '0.1.0' } })
	];

	// Mock operations with no changesets
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: false }),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async () => null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// Should have errors for production cycles
	assert.ok(plan.errors.some((e) => e.includes('Production dependency cycle')));

	// Should not compute publishing order
	assert.strictEqual(plan.publishing_order.length, 0);
});

test('warns when MAX_ITERATIONS reached without convergence', async () => {
	// Create a very deep dependency chain (12 levels) with breaking changes
	// This will require more than 10 iterations to fully propagate
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'level-1', version: '0.1.0' }),
		create_mock_repo({ name: 'level-2', version: '0.1.0', deps: { 'level-1': '^0.1.0' } }),
		create_mock_repo({ name: 'level-3', version: '0.1.0', deps: { 'level-2': '^0.1.0' } }),
		create_mock_repo({ name: 'level-4', version: '0.1.0', deps: { 'level-3': '^0.1.0' } }),
		create_mock_repo({ name: 'level-5', version: '0.1.0', deps: { 'level-4': '^0.1.0' } }),
		create_mock_repo({ name: 'level-6', version: '0.1.0', deps: { 'level-5': '^0.1.0' } }),
		create_mock_repo({ name: 'level-7', version: '0.1.0', deps: { 'level-6': '^0.1.0' } }),
		create_mock_repo({ name: 'level-8', version: '0.1.0', deps: { 'level-7': '^0.1.0' } }),
		create_mock_repo({ name: 'level-9', version: '0.1.0', deps: { 'level-8': '^0.1.0' } }),
		create_mock_repo({ name: 'level-10', version: '0.1.0', deps: { 'level-9': '^0.1.0' } }),
		create_mock_repo({ name: 'level-11', version: '0.1.0', deps: { 'level-10': '^0.1.0' } }),
		create_mock_repo({ name: 'level-12', version: '0.1.0', deps: { 'level-11': '^0.1.0' } })
	];

	// Mock operations: only level-1 has a changeset with breaking change
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'level-1'
		}),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => {
			if (options.repo.library.name === 'level-1') {
				// Breaking change in 0.x (minor bump)
				return { ok: true, version: '0.2.0', bump_type: 'minor' as const };
			}
			return null;
		}
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// Should have a warning about MAX_ITERATIONS
	const convergence_warning = plan.warnings.find((w) => w.includes('Reached maximum iterations'));
	assert.ok(convergence_warning !== undefined);

	// Warning names what one more pass would change, and guesses nothing further
	assert.ok(convergence_warning.includes('package(s) may still need processing'));
	assert.ok(convergence_warning.includes('level-12'));
	assert.ok(!convergence_warning.includes('Estimated'));

	// Should still have produced some version changes (just not all of them)
	assert.ok(plan.version_changes.length > 0);
	assert.ok(plan.version_changes.length < repos.length); // Not all processed
});

test('does not warn when the last allowed iteration converges the plan', async () => {
	// One level per iteration: the last allowed one adds the chain's end, so it still
	// reports a change, but one more pass would change nothing
	const repos: Array<LocalRepo> = Array.from(
		{ length: GITOPS_MAX_ITERATIONS_DEFAULT + 1 },
		(_, i) =>
			create_mock_repo({
				name: `level-${i + 1}`,
				version: '0.1.0',
				deps: i === 0 ? undefined : { [`level-${i}`]: '^0.1.0' }
			})
	);
	const mock_ops: ChangesetOperations = {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'level-1'
		}),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) =>
			options.repo.library.name === 'level-1'
				? { ok: true, version: '0.2.0', bump_type: 'minor' as const }
				: null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops, verbose: true });

	assert.strictEqual(plan.verbose_data?.total_iterations, GITOPS_MAX_ITERATIONS_DEFAULT);
	assert.strictEqual(plan.version_changes.length, repos.length);
	assert.deepEqual(plan.warnings, []);
});

test('excludes non-npm (cargo) repos from the plan', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '1.0.0' }),
		create_mock_repo({ name: 'rust-tool', version: '0.1.0', kind: 'cargo' })
	];

	// Even when the cargo repo "has changesets", it must be filtered out before any
	// changeset processing — so the filter, not the private/changeset logic, excludes it.
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: true }),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => ({
			ok: true,
			version: options.repo.library.name === 'pkg-a' ? '1.0.1' : '0.1.1',
			bump_type: 'patch' as const
		})
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	// The npm package still plans normally...
	assert.ok(plan.publishing_order.includes('pkg-a'));
	assert.ok(plan.version_changes.some((vc) => vc.package_name === 'pkg-a'));
	// ...while the cargo repo is absent from the order and version changes...
	assert.ok(!plan.publishing_order.includes('rust-tool'));
	assert.ok(!plan.version_changes.some((vc) => vc.package_name === 'rust-tool'));
	// ...and the exclusion is reported rather than silent.
	assert.ok(plan.info.some((line) => line.includes('non-npm') && line.includes('rust-tool')));
	// The sentence stays out of the package-name list
	assert.deepEqual(plan.no_changes, []);
});

test('lists packages with nothing to publish apart from info sentences', async () => {
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'pkg-a', version: '0.1.0', dev_deps: { 'pkg-b': '0.1.0' } }),
		create_mock_repo({ name: 'pkg-b', version: '0.1.0', dev_deps: { 'pkg-a': '0.1.0' } })
	];
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: false }),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async () => null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	assert.deepEqual(plan.no_changes, ['pkg-a', 'pkg-b']);
	assert.deepEqual(plan.info, [
		'1 dev dependency cycle(s) detected (normal, shown in gitops_analyze)'
	]);
});

// `app` is listed before `lib`, which escalates to breaking only when the iteration reaches
// it, so `app` gets its auto-changeset first at patch and is raised a pass later.
const create_config_order_case = (): { repos: Array<LocalRepo>; ops: ChangesetOperations } => ({
	repos: [
		create_mock_repo({ name: 'app', version: '0.3.0', deps: { lib: '^0.2.0' } }),
		create_mock_repo({ name: 'lib', version: '0.2.0', deps: { core: '^0.1.0' } }),
		create_mock_repo({ name: 'core', version: '0.1.0' })
	],
	ops: {
		has_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name !== 'app'
		}),
		read_changesets: async () => ({ ok: true, value: [] }),
		predict_next_version: async (options) => {
			const name = options.repo.library.name;
			if (name === 'core') return { ok: true, version: '0.2.0', bump_type: 'minor' as const };
			if (name === 'lib') return { ok: true, version: '0.2.1', bump_type: 'patch' as const };
			return null;
		}
	}
});

test('an auto-changeset raised by a dependent listed before its dependency stays auto', async () => {
	const { repos, ops } = create_config_order_case();

	const plan = await generate_publishing_plan(repos, { ops });

	assert.deepEqual(plan.publishing_order, ['core', 'lib', 'app']);
	const kinds = new Map(plan.version_changes.map((vc) => [vc.package_name, vc]));

	const lib = kinds.get('lib');
	assert.ok(lib);
	assert.strictEqual(version_change_kind(lib), 'escalation');
	assert.strictEqual(lib.existing_bump, 'patch');
	assert.strictEqual(lib.to, '0.3.0');

	const app = kinds.get('app');
	assert.ok(app);
	assert.strictEqual(version_change_kind(app), 'auto');
	assert.strictEqual(app.will_generate_changeset, true);
	assert.strictEqual(app.needs_bump_escalation, undefined);
	assert.strictEqual(app.existing_bump, undefined);
	assert.strictEqual(app.bump_type, 'minor');
	assert.strictEqual(app.to, '0.4.0');
	assert.strictEqual(app.breaking, true);

	const via = new Map(
		derive_publish_steps(plan).flatMap((s) => (s.kind === 'publish' ? [[s.repo, s.via]] : []))
	);
	assert.strictEqual(via.get('app'), 'auto');
});

test('the logged plan lists each change once', async () => {
	const { repos, ops } = create_config_order_case();
	const plan = await generate_publishing_plan(repos, { ops });

	const log = create_stream_log();
	log_publishing_plan(plan, log);

	const positions = log.lines.filter((line) => /\[\d+\/\d+\]/.test(line));
	assert.deepEqual(
		positions.map((line) => /\[(\d+\/\d+)\]/.exec(line)![1]),
		['1/3', '2/3', '3/3']
	);
	assert.strictEqual(log.lines.filter((line) => line.includes('changesets specify')).length, 1);
	assert.ok(positions.some((line) => line.includes('app:') && line.includes('[auto-changeset]')));
});

test('warns on a repo whose changesets yield no bump for its package', async () => {
	const named_other = parse_changeset_content('---\n"other": patch\n---\n\nfix\n', 'other.md');
	assert.ok(named_other);
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'unnamed', version: '0.1.0' }),
		create_mock_repo({ name: 'unparsed', version: '0.1.0' })
	];
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: true }),
		read_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'unnamed' ? [named_other] : []
		}),
		predict_next_version: async () => null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	assert.deepEqual(plan.version_changes, []);
	assert.deepEqual(plan.no_changes, []);
	assert.strictEqual(plan.warnings.length, 2);
	const unnamed = plan.warnings.find((w) => w.startsWith('unnamed '));
	assert.ok(unnamed);
	assert.ok(unnamed.includes('none that parses names unnamed'));
	assert.ok(unnamed.includes('will not publish'));
	const unparsed = plan.warnings.find((w) => w.startsWith('unparsed '));
	assert.ok(unparsed?.includes('none parses'));
});

test('warns on a repo whose changeset files could not be read', async () => {
	const repos: Array<LocalRepo> = [create_mock_repo({ name: 'unreadable', version: '0.1.0' })];
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: true }),
		read_changesets: async () => ({ ok: false, message: 'EACCES' }),
		predict_next_version: async () => null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	assert.deepEqual(plan.version_changes, []);
	assert.deepEqual(plan.errors, []);
	assert.strictEqual(plan.warnings.length, 1);
	const warning = plan.warnings[0]!;
	assert.ok(warning.startsWith('unreadable has changeset files that could not be read (EACCES)'));
	assert.ok(warning.includes('will not publish'));
});

test('a repo whose changesets yield no bump is warned it publishes only as an auto change', async () => {
	const named_old = parse_changeset_content('---\n"old-name": patch\n---\n\nfix\n', 'old.md');
	assert.ok(named_old);
	const repos: Array<LocalRepo> = [
		create_mock_repo({ name: 'app', version: '0.3.0', deps: { lib: '^0.2.0' } }),
		create_mock_repo({ name: 'lib', version: '0.2.0' })
	];
	const mock_ops: ChangesetOperations = {
		has_changesets: async () => ({ ok: true, value: true }),
		read_changesets: async (options) => ({
			ok: true,
			value: options.repo.library.name === 'app' ? [named_old] : []
		}),
		predict_next_version: async (options) =>
			options.repo.library.name === 'lib'
				? { ok: true, version: '0.3.0', bump_type: 'minor' as const }
				: null
	};

	const plan = await generate_publishing_plan(repos, { ops: mock_ops });

	const app = plan.version_changes.find((vc) => vc.package_name === 'app');
	assert.ok(app);
	assert.strictEqual(version_change_kind(app), 'auto');
	assert.strictEqual(plan.warnings.length, 1);
	const warning = plan.warnings[0]!;
	assert.ok(warning.startsWith('app has changeset files, but none that parses names app'));
	assert.ok(warning.includes('publishes only as the auto-generated change'));
	assert.ok(!warning.includes('will not publish'));
	assert.ok(warning.includes('check the package names in their frontmatter'));
});

test('checks each repo for changesets once', async () => {
	const { repos, ops } = create_config_order_case();
	const checked: Array<string> = [];
	const plan = await generate_publishing_plan(repos, {
		ops: {
			...ops,
			has_changesets: async (options) => {
				checked.push(options.repo.library.name);
				return ops.has_changesets(options);
			}
		}
	});

	assert.strictEqual(plan.version_changes.length, 3);
	assert.deepEqual([...checked].sort(), ['app', 'core', 'lib']);
});
