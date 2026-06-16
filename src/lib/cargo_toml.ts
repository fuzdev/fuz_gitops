import {existsSync} from 'node:fs';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';

/**
 * The handful of identity fields gitops reads from a Rust repo's `Cargo.toml`
 * to render it on the dashboard. Everything is optional — a workspace root has
 * no `name`, and any field may be absent or inherited.
 */
export interface CargoMetadata {
	name?: string;
	version?: string;
	description?: string;
	repository?: string;
}

/** The `Cargo.toml` tables that carry package identity (single-crate vs workspace root). */
const CARGO_METADATA_SECTIONS: ReadonlySet<string> = new Set(['package', 'workspace.package']);
const CARGO_METADATA_KEYS = ['name', 'version', 'description', 'repository'] as const;
type CargoMetadataKey = (typeof CARGO_METADATA_KEYS)[number];

/**
 * Best-effort read of a repo's root `Cargo.toml` for the identity fields the
 * gitops dashboard renders. Returns `null` when there's no `Cargo.toml`.
 *
 * @param repo_dir - absolute path to the repo
 */
export const cargo_toml_load = async (repo_dir: string): Promise<CargoMetadata | null> => {
	const cargo_toml_path = join(repo_dir, 'Cargo.toml');
	if (!existsSync(cargo_toml_path)) return null;
	return cargo_toml_parse(await readFile(cargo_toml_path, 'utf8'));
};

/**
 * Extracts `name`/`version`/`description`/`repository` from the `[package]` and
 * `[workspace.package]` tables of a `Cargo.toml`.
 *
 * Deliberately not a full TOML parser: it scans for simple `key = "value"`
 * string entries in those two tables, which covers both a single-crate manifest
 * and a workspace root. Inline-table values like `version = { workspace = true }`
 * (and the equivalent `version.workspace = true`) are ignored — a member crate
 * inheriting from the workspace has no literal here, and gitops only ever reads a
 * repo's root manifest, where these are concrete. The first non-empty value for a
 * key wins, so a top-level `[package]` takes precedence over `[workspace.package]`
 * when both appear.
 */
export const cargo_toml_parse = (contents: string): CargoMetadata => {
	const metadata: CargoMetadata = {};
	let in_metadata_section = false;
	for (const raw_line of contents.split('\n')) {
		const line = raw_line.trim();
		if (line === '' || line.startsWith('#')) continue;
		// Section header — only `[package]`/`[workspace.package]` arm value capture.
		if (line.startsWith('[')) {
			const end = line.indexOf(']');
			in_metadata_section = end !== -1 && CARGO_METADATA_SECTIONS.has(line.slice(1, end).trim());
			continue;
		}
		if (!in_metadata_section) continue;
		const eq = line.indexOf('=');
		if (eq === -1) continue;
		const key = line.slice(0, eq).trim();
		if (!CARGO_METADATA_KEYS.includes(key as CargoMetadataKey)) continue;
		if (metadata[key as CargoMetadataKey] !== undefined) continue; // first value wins
		const value = cargo_toml_string_value(line.slice(eq + 1).trim());
		if (value !== null) metadata[key as CargoMetadataKey] = value;
	}
	return metadata;
};

/**
 * Reads a simple single- or double-quoted TOML string, ignoring any trailing
 * inline comment. Returns `null` for non-string values (inline tables, arrays,
 * booleans, numbers), which the caller skips.
 */
const cargo_toml_string_value = (raw: string): string | null => {
	const quote = raw[0];
	if (quote !== '"' && quote !== "'") return null;
	const end = raw.indexOf(quote, 1);
	if (end === -1) return null;
	return raw.slice(1, end);
};
