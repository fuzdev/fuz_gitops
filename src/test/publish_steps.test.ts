import {assert, test, describe} from 'vitest';

import {derive_publish_steps, format_publish_steps} from '$lib/publish_steps.ts';
import type {PublishingPlan, VersionChange, DependencyUpdate} from '$lib/publishing_plan.ts';

const make_version_change = (
	overrides: Partial<VersionChange> & {package_name: string},
): VersionChange => ({
	from: '1.0.0',
	to: '1.0.1',
	bump_type: 'patch',
	breaking: false,
	has_changesets: true,
	...overrides,
});

const make_update = (
	dependent: string,
	dependency: string,
	type: DependencyUpdate['type'],
): DependencyUpdate => ({
	dependent_package: dependent,
	updated_dependency: dependency,
	current_version: '^1.0.0',
	new_version: 'ignored', // derive reads the dependency's plan version, not this
	type,
});

const make_plan = (overrides: Partial<PublishingPlan> = {}): PublishingPlan => ({
	publishing_order: [],
	version_changes: [],
	dependency_updates: [],
	breaking_cascades: new Map(),
	warnings: [],
	info: [],
	errors: [],
	...overrides,
});

// core (explicit changeset, breaking) → mid (auto-changeset) → app (escalation);
// tool publishes and is a dev dep of consumer (which itself doesn't publish).
const make_cascade_plan = (): PublishingPlan =>
	make_plan({
		publishing_order: ['core', 'tool', 'mid', 'app'],
		version_changes: [
			make_version_change({
				package_name: 'core',
				from: '0.5.0',
				to: '0.6.0',
				bump_type: 'minor',
				breaking: true,
				has_changesets: true,
			}),
			make_version_change({
				package_name: 'tool',
				from: '1.0.0',
				to: '1.0.1',
				bump_type: 'patch',
				has_changesets: true,
			}),
			make_version_change({
				package_name: 'mid',
				from: '0.3.0',
				to: '0.4.0',
				bump_type: 'minor',
				breaking: true,
				has_changesets: false,
				will_generate_changeset: true,
			}),
			make_version_change({
				package_name: 'app',
				from: '0.2.0',
				to: '0.3.0',
				bump_type: 'minor',
				breaking: true,
				has_changesets: true,
				needs_bump_escalation: true,
			}),
		],
		dependency_updates: [
			make_update('mid', 'core', 'dependencies'),
			make_update('app', 'mid', 'dependencies'),
			make_update('consumer', 'tool', 'devDependencies'),
		],
	});

describe('derive_publish_steps', () => {
	test('classifies each publish by how its bump arises', () => {
		const steps = derive_publish_steps(make_cascade_plan());
		const publishes = steps.filter((s) => s.kind === 'publish');
		const via = new Map(publishes.map((s) => [s.repo, s.via]));
		assert.strictEqual(via.get('core'), 'changeset');
		assert.strictEqual(via.get('mid'), 'auto_changeset');
		assert.strictEqual(via.get('app'), 'escalation');
		assert.strictEqual(via.get('tool'), 'changeset');
	});

	test('prod dependency updates carry the published version and create a changeset', () => {
		const steps = derive_publish_steps(make_cascade_plan());
		const update = steps.find((s) => s.kind === 'dependency_update' && s.dependent === 'mid');
		assert.ok(update && update.kind === 'dependency_update');
		assert.strictEqual(update.dependency, 'core');
		assert.strictEqual(update.to, '0.6.0');
		assert.strictEqual(update.dep_type, 'prod');
		assert.strictEqual(update.creates_changeset, true);
	});

	test('dev-dependency updates appear without a changeset and do not publish the dependent', () => {
		const steps = derive_publish_steps(make_cascade_plan());
		const dev = steps.find((s) => s.kind === 'dev_dep_update');
		assert.ok(dev && dev.kind === 'dev_dep_update');
		assert.strictEqual(dev.repo, 'consumer');
		assert.strictEqual(dev.dependency, 'tool');
		assert.strictEqual(dev.to, '1.0.1');
		// consumer only carries a dev dep, so it never publishes
		assert.ok(!steps.some((s) => s.kind === 'publish' && s.repo === 'consumer'));
	});

	test('deploy is omitted by default and includes every changed repo when enabled', () => {
		const without = derive_publish_steps(make_cascade_plan());
		assert.ok(!without.some((s) => s.kind === 'deploy'));

		const withDeploy = derive_publish_steps(make_cascade_plan(), {deploy: true});
		const deployed = withDeploy.filter((s) => s.kind === 'deploy').map((s) => s.repo);
		// published (core, tool, mid, app) + the dev-dep dependent (consumer)
		assert.deepEqual([...deployed].sort(), ['app', 'consumer', 'core', 'mid', 'tool']);
		assert.ok(withDeploy.filter((s) => s.kind === 'deploy').every((s) => s.builds));
	});

	test('a dependency that does not publish is not propagated to its dependents', () => {
		// `lib` is referenced by `app` but absent from version_changes (nothing published it),
		// so no dependency_update should be derived.
		const plan = make_plan({
			publishing_order: ['app'],
			version_changes: [make_version_change({package_name: 'app'})],
			dependency_updates: [make_update('app', 'lib', 'dependencies')],
		});
		const steps = derive_publish_steps(plan);
		assert.ok(!steps.some((s) => s.kind === 'dependency_update'));
	});
});

describe('format_publish_steps', () => {
	test('renders one line per step', () => {
		const lines = format_publish_steps(derive_publish_steps(make_cascade_plan(), {deploy: true}));
		assert.ok(lines.length > 0);
		assert.ok(lines.some((l) => l.includes('publish') && l.includes('core')));
		assert.ok(lines.some((l) => l.startsWith('dev dep')));
	});

	test('reports an empty plan explicitly', () => {
		assert.deepEqual(format_publish_steps([]), ['(no side effects — nothing to publish)']);
	});
});
