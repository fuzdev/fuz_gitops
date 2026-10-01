import type { Logger } from '@fuzdev/fuz_util/log.ts';
import { writeFile } from 'node:fs/promises';

export type OutputFormat = 'stdout' | 'json' | 'markdown';

/**
 * Writes one machine-readable document, or one line of a stream, to stdout,
 * ending it with a newline.
 */
export type WriteStdout = (content: string) => void;

export interface OutputOptions {
	format: OutputFormat;
	outfile?: string;
	log?: Logger;
	/**
	 * Where a `json` or `markdown` document goes without `outfile`; the one
	 * `route_human_output` returns, so the document reaches stdout while the
	 * logger's lines go to stderr.
	 *
	 * @default `console.log`
	 */
	write_stdout?: WriteStdout;
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
 * Whether a task's stdout carries a machine-readable document: a `json` or
 * `markdown` report not sent to `--outfile`.
 *
 * @param format - the task's `--format`
 * @param outfile - the task's `--outfile`, if any
 */
export const output_is_machine = (format: string, outfile: string | undefined): boolean =>
	(format === 'json' || format === 'markdown') && !outfile;

/**
 * Keeps a task's stdout for its machine-readable document or stream when
 * `machine` is set: the logger's `info` and `debug` lines, which it writes with
 * `console.log`, go where its errors go — stderr — and child loggers inherit
 * that. Gro hands a task the logger its own lines go through, so gro's lines
 * after the task runs (`✓`, the timings) follow to stderr; the two it prints
 * before the task runs (`invoking`, `→ <task>`) stay on stdout, out of the
 * task's reach.
 *
 * @param log - the task's logger
 * @param machine - whether stdout carries a machine-readable document or stream
 * @returns writes to the stdout the logger wrote to before, for the document
 * @mutates log - overrides its `console` when `machine` is set
 */
export const route_human_output = (log: Logger, machine: boolean): WriteStdout => {
	const stdout = log.console;
	if (machine) {
		log.console = {
			log: (...args) => stdout.error(...args),
			warn: (...args) => stdout.warn(...args),
			error: (...args) => stdout.error(...args)
		};
	}
	return (content) => stdout.log(content);
};

/**
 * Formats data and outputs to file or stdout based on options.
 *
 * Supports three formats:
 * - stdout: Uses logger for colored/styled output (cannot use with `--outfile`)
 * - json: Stringified JSON
 * - markdown: Formatted markdown text
 *
 * @throws {Error} if stdout format used with `outfile`, or if logger missing for stdout
 */
export const format_and_output = async <T>(
	data: T,
	formatters: OutputFormatters<T>,
	options: OutputOptions
): Promise<void> => {
	const { format, outfile, log, write_stdout = write_console_log } = options;

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

	// Output to file or stdout
	if (outfile) {
		await writeFile(outfile, content);
		log?.info(`Output written to ${outfile}`);
	} else {
		// Write raw to stdout in one shot. Routing through `log` would prefix every
		// line with `[taskname]`, corrupting JSON and cluttering markdown — keep the
		// machine-readable formats unprefixed and pipeable (use `--outfile` for output
		// fully free of Gro's own preamble).
		write_stdout(content);
	}
};

// eslint-disable-next-line no-console
const write_console_log: WriteStdout = (content) => console.log(content);
