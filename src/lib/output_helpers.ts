import type {Logger} from '@fuzdev/fuz_util/log.js';
import {writeFile} from 'node:fs/promises';

export type OutputFormat = 'stdout' | 'json' | 'markdown';

export interface OutputOptions {
	format: OutputFormat;
	outfile?: string;
	log?: Logger;
}

export interface OutputFormatters<T> {
	json: (data: T) => string;
	markdown: (data: T) => Array<string>;
	/**
	 * This function should call log methods directly for colored/styled output.
	 */
	stdout: (data: T, log: Logger) => void;
}

/**
 * Formats data and outputs to file or stdout based on options.
 *
 * Supports three formats:
 * - stdout: Uses logger for colored/styled output (cannot use with --outfile)
 * - json: Stringified JSON
 * - markdown: Formatted markdown text
 *
 * @throws {Error} if stdout format used with outfile, or if logger missing for stdout
 */
export const format_and_output = async <T>(
	data: T,
	formatters: OutputFormatters<T>,
	options: OutputOptions,
): Promise<void> => {
	const {format, outfile, log} = options;

	// Handle stdout format (special case - uses logger directly)
	if (format === 'stdout') {
		if (outfile) {
			throw new Error('--outfile is not supported with stdout format, use json or markdown');
		}
		if (!log) {
			throw new Error('Logger is required for stdout format');
		}
		formatters.stdout(data, log);
		return;
	}

	// Format data
	const content = format === 'json' ? formatters.json(data) : formatters.markdown(data).join('\n');

	// Output to file or log
	if (outfile) {
		await writeFile(outfile, content);
		log?.info(`Output written to ${outfile}`);
	} else {
		// Log line by line for better formatting
		const lines = content.split('\n');
		for (const line of lines) {
			log?.info(line);
		}
	}
};
