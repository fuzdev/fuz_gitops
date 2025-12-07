import type {Logger} from '@fuzdev/fuz_util/log.js';
import {styleText as st} from 'node:util';

import type {DependencyGraphBuilder} from './dependency_graph.js';

/**
 * Formats wildcard dependencies as styled strings.
 * Returns array of lines for inclusion in output.
 */
export const format_wildcard_dependencies = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
): Array<string> => {
	if (analysis.wildcard_deps.length === 0) return [];

	const lines: Array<string> = [];
	lines.push(st('yellow', `\n⚠️  Found ${analysis.wildcard_deps.length} wildcard dependencies:`));
	for (const {pkg, dep, version} of analysis.wildcard_deps) {
		lines.push(`  ${pkg} → ${dep} ${st('red', version)}`);
	}
	return lines;
};

/**
 * Formats dev circular dependencies as styled strings.
 * Returns array of lines for inclusion in output.
 */
export const format_dev_cycles = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
): Array<string> => {
	if (analysis.dev_cycles.length === 0) return [];

	const lines: Array<string> = [];
	lines.push(
		st(
			'dim',
			`\nℹ️  Found ${analysis.dev_cycles.length} dev circular dependencies (normal, non-blocking):`,
		),
	);
	for (const cycle of analysis.dev_cycles) {
		lines.push(st('dim', `  ${cycle.join(' → ')}`));
	}
	return lines;
};

/**
 * Formats production/peer circular dependencies as styled strings.
 * Returns array of lines for inclusion in output.
 */
export const format_production_cycles = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
): Array<string> => {
	if (analysis.production_cycles.length === 0) return [];

	const lines: Array<string> = [];
	lines.push(
		st(
			'red',
			`\n❌ Found ${analysis.production_cycles.length} production/peer circular dependencies (blocks publishing):`,
		),
	);
	for (const cycle of analysis.production_cycles) {
		lines.push(`  ${st('red', cycle.join(' → '))}`);
	}
	return lines;
};

/**
 * Logs wildcard dependencies as warnings.
 * Wildcard dependencies require attention and should be reviewed.
 */
export const log_wildcard_dependencies = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	log: Logger,
	indent = '',
): void => {
	const lines = format_wildcard_dependencies(analysis);
	for (const line of lines) {
		log.warn(indent + line);
	}
};

/**
 * Logs dev circular dependencies as info.
 * Dev cycles are normal and non-blocking, so they're informational, not warnings.
 */
export const log_dev_cycles = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	log: Logger,
	indent = '',
): void => {
	const lines = format_dev_cycles(analysis);
	for (const line of lines) {
		log.info(indent + line);
	}
};

/**
 * Logs production/peer circular dependencies as errors.
 * Production cycles block publishing and must be resolved.
 */
export const log_production_cycles = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	log: Logger,
	indent = '',
): void => {
	const lines = format_production_cycles(analysis);
	for (const line of lines) {
		log.error(indent + line);
	}
};

/**
 * Logs all dependency analysis results (wildcards, production cycles, dev cycles).
 * Convenience function that calls all three logging functions in order.
 */
export const log_dependency_analysis = (
	analysis: ReturnType<DependencyGraphBuilder['analyze']>,
	log: Logger,
	indent = '',
): void => {
	log_wildcard_dependencies(analysis, log, indent);
	log_production_cycles(analysis, log, indent);
	log_dev_cycles(analysis, log, indent);
};

/**
 * Logs a simple bulleted list with a header.
 * Common pattern for warnings, info messages, and other lists.
 */
export const log_list = (
	items: Array<string>,
	header: string,
	color: 'cyan' | 'yellow' | 'red' | 'dim',
	log: Logger,
	log_method: 'info' | 'warn' | 'error' = 'info',
): void => {
	if (items.length === 0) return;

	// Only add newline prefix if header is non-empty
	const formatted_header = header ? `\n${header}` : '';
	if (formatted_header) {
		log[log_method](st(color, formatted_header));
	}
	for (const item of items) {
		log[log_method](st(color, `  • ${item}`));
	}
};
