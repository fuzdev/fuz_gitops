import type {Task} from '@ryanatkn/gro';
import {z} from 'zod';
import {styleText as st} from 'node:util';
import type {Logger} from '@fuzdev/fuz_util/log.js';

import {get_gitops_ready} from './gitops_task_helpers.js';
import {type DependencyGraph, DependencyGraphBuilder} from './dependency_graph.js';
import type {LocalRepo} from './local_repo.js';
import {validate_dependency_graph} from './graph_validation.js';
import {
	format_wildcard_dependencies,
	format_dev_cycles,
	format_production_cycles,
} from './log_helpers.js';
import {format_and_output, type OutputFormatters} from './output_helpers.js';
import {GITOPS_CONFIG_PATH_DEFAULT} from './gitops_constants.js';

/** @nodocs */
export const Args = z.strictObject({
	config: z
		.string()
		.meta({description: 'path to the gitops config file, absolute or relative to the cwd'})
		.default(GITOPS_CONFIG_PATH_DEFAULT),
	dir: z
		.string()
		.meta({description: 'path containing the repos, defaults to the parent of the config dir'})
		.optional(),
	format: z
		.enum(['stdout', 'json', 'markdown'])
		.meta({description: 'output format'})
		.default('stdout'),
	outfile: z.string().meta({description: 'write output to file instead of logging'}).optional(),
});
export type Args = z.infer<typeof Args>;

/** @nodocs */
export const task: Task<Args> = {
	Args,
	summary: 'analyze dependency structure and relationships across repos',
	run: async ({args, log}) => {
		const {config, dir, format, outfile} = args;

		// Get repos ready (without downloading)
		const {local_repos} = await get_gitops_ready({config, dir, download: false, log});

		// Build dependency graph and validate (but don't throw on cycles for analyze)
		const {graph, publishing_order: order} = validate_dependency_graph(local_repos, {
			log,
			throw_on_prod_cycles: false, // Analyze should report, not throw
			log_cycles: false, // We'll show cycles in our formatted output
			log_order: false, // We'll show order in our formatted output
		});

		// Perform additional analysis
		const builder = new DependencyGraphBuilder();
		const analysis = builder.analyze(graph);

		// Publishing order (may be null if prod cycles exist)
		const publishing_order = order.length > 0 ? order : null;

		// Format and output using output_helpers
		const data = {
			repos: local_repos,
			graph,
			analysis,
			publishing_order,
		};

		await format_and_output(data, create_formatters(), {format, outfile, log});
	},
};

// Data type for analysis output
interface AnalysisData {
	repos: Array<LocalRepo>;
	graph: DependencyGraph;
	analysis: ReturnType<DependencyGraphBuilder['analyze']>;
	publishing_order: Array<string> | null;
}

// Create formatters for output_helpers
const create_formatters = (): OutputFormatters<AnalysisData> => ({
	json: (data) => format_json(data.graph, data.analysis, data.publishing_order),
	markdown: (data) => format_markdown(data.repos, data.graph, data.analysis, data.publishing_order),
	stdout: (data, log) =>
		format_stdout(data.repos, data.graph, data.analysis, data.publishing_order, log),
});

// Helper to calculate common statistics
const calculate_stats = (graph: DependencyGraph) => {
	const total_deps = Array.from(graph.nodes.values()).reduce(
		(sum, node) => sum + node.dependencies.size,
		0,
	);
	const internal_deps = Array.from(graph.nodes.values()).reduce(
		(sum, node) =>
			sum + Array.from(node.dependencies.keys()).filter((name) => graph.nodes.has(name)).length,
		0,
	);
	return {total_deps, internal_deps};
};

const format_json = (
	graph: DependencyGraph,
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	publishing_order: Array<string> | null,
): string => {
	const output = {
		graph: graph.toJSON(),
		analysis,
		publishing_order,
	};
	return JSON.stringify(output, null, 2);
};

const format_markdown = (
	repos: Array<LocalRepo>,
	graph: DependencyGraph,
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	publishing_order: Array<string> | null,
): Array<string> => {
	const lines: Array<string> = ['# Dependency Analysis'];

	// Summary stats
	const {total_deps, internal_deps} = calculate_stats(graph);

	lines.push('', '## Summary', '');
	lines.push(`- **Total packages**: ${repos.length}`);
	lines.push(`- **Total dependencies**: ${total_deps}`);
	lines.push(`- **Internal dependencies**: ${internal_deps}`);
	lines.push(`- **Wildcard dependencies**: ${analysis.wildcard_deps.length}`);
	lines.push(`- **Production/peer cycles**: ${analysis.production_cycles.length}`);
	lines.push(`- **Dev cycles**: ${analysis.dev_cycles.length}`);

	// Publishing order
	if (publishing_order) {
		lines.push('', '## Publishing Order', '');
		publishing_order.forEach((name, i) => {
			const node = graph.get_node(name);
			const version = node ? `v${node.version}` : '';
			lines.push(`${i + 1}. \`${name}\` ${version}`);
		});
	}

	// Cycles (show problems first)
	if (analysis.production_cycles.length > 0) {
		lines.push('', '## ❌ Production/Peer Circular Dependencies', '');
		lines.push('> **These block publishing and must be resolved!**');
		lines.push('');
		for (const cycle of analysis.production_cycles) {
			lines.push(`- ${cycle.map((n) => `\`${n}\``).join(' → ')}`);
		}
	}

	if (analysis.dev_cycles.length > 0) {
		lines.push('', '## ⚠️ Dev Circular Dependencies', '');
		lines.push('> These are normal and do not block publishing.');
		lines.push('');
		for (const cycle of analysis.dev_cycles) {
			lines.push(`- ${cycle.map((n) => `\`${n}\``).join(' → ')}`);
		}
	}

	// Wildcard dependencies
	if (analysis.wildcard_deps.length > 0) {
		lines.push('', '## ⚠️ Wildcard Dependencies', '');
		lines.push('| Package | Dependency | Version |');
		lines.push('|---------|------------|---------|');
		for (const {pkg, dep, version} of analysis.wildcard_deps) {
			lines.push(`| \`${pkg}\` | \`${dep}\` | \`${version}\` |`);
		}
	}

	// Dependency graph (simplified)
	lines.push('', '## Internal Dependencies', '');
	for (const node of graph.nodes.values()) {
		const internal_deps = Array.from(node.dependencies.entries()).filter(([name]) =>
			graph.nodes.has(name),
		);
		if (internal_deps.length > 0) {
			lines.push(`- **${node.name}**`);
			for (const [dep_name, spec] of internal_deps) {
				const badge = spec.type === 'peer' ? '(peer)' : spec.type === 'dev' ? '(dev)' : '';
				lines.push(`  - ${dep_name} ${badge}`);
			}
		}
	}

	return lines;
};

const format_stdout = (
	repos: Array<LocalRepo>,
	graph: DependencyGraph,
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	publishing_order: Array<string> | null,
	log: Logger,
): void => {
	log.info(st('cyan', `📊 Analyzing ${repos.length} repositories...`));

	// Publishing order
	if (publishing_order) {
		log.info(st('yellow', 'Publishing order:'));
		publishing_order.forEach((name, i) => {
			const node = graph.get_node(name);
			const version = node ? node.version : 'unknown';
			log.info(`  ${st('dim', `${i + 1}.`)} ${name} ${st('dim', `(${version})`)}`);
		});
		log.info('');
	}

	// Dependencies summary
	log.info(st('yellow', 'Dependency relationships:'));
	for (const node of graph.nodes.values()) {
		const internal_deps = Array.from(node.dependencies.entries()).filter(([name]) =>
			graph.nodes.has(name),
		);
		if (internal_deps.length > 0) {
			log.info(`  ${st('cyan', node.name)}`);
			for (const [dep_name, spec] of internal_deps) {
				const type_color = spec.type === 'peer' ? 'magenta' : spec.type === 'dev' ? 'dim' : 'white';
				log.info(
					`    ${st(type_color, '→')} ${dep_name} ${st('dim', `(${spec.type}: ${spec.version})`)}`,
				);
			}
		}
	}
	log.info('');

	// Dependency analysis
	for (const line of format_wildcard_dependencies(analysis)) {
		log.info(line);
	}
	for (const line of format_production_cycles(analysis)) {
		log.info(line);
	}
	for (const line of format_dev_cycles(analysis)) {
		log.info(line);
	}

	// Success message based on cycle detection
	const has_prod_cycles = analysis.production_cycles.length > 0;
	const has_dev_cycles = analysis.dev_cycles.length > 0;

	if (!has_prod_cycles && !has_dev_cycles) {
		log.info(st('green', '✅ No circular dependencies detected'));
	} else if (!has_prod_cycles) {
		log.info(st('green', '✓ Publishing order computed successfully (dev deps excluded)'));
	}

	// Summary
	const {total_deps, internal_deps} = calculate_stats(graph);

	log.info('');
	log.info(st('cyan', 'Summary:'));
	log.info(`  Total packages: ${repos.length}`);
	log.info(`  Total dependencies: ${total_deps}`);
	log.info(`  Internal dependencies: ${internal_deps}`);
	log.info(`  Wildcard dependencies: ${analysis.wildcard_deps.length}`);
	log.info(`  Production/peer circular dependencies: ${analysis.production_cycles.length}`);
	log.info(`  Dev circular dependencies: ${analysis.dev_cycles.length}`);
};
