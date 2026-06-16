import {assert, describe, test} from 'vitest';

import {cargo_toml_parse} from '$lib/cargo_toml.ts';

describe('cargo_toml_parse', () => {
	test('reads identity from a workspace root `[workspace.package]`', () => {
		const metadata = cargo_toml_parse(
			`[workspace]
resolver = "2"
members = [
    "crates/foo",
    "crates/bar",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"
repository = "https://github.com/fuzdev/tsv"

[workspace.dependencies]
foo = { path = "crates/foo" }
serde = "1"
`,
		);
		// A workspace root has no `name`, and dependency tables are ignored.
		assert.deepStrictEqual(metadata, {
			version: '0.1.0',
			repository: 'https://github.com/fuzdev/tsv',
		});
	});

	test('reads identity from a single-crate `[package]`', () => {
		const metadata = cargo_toml_parse(
			`[package]
name = "my_crate"
version = "1.2.3"
description = "does a thing"
repository = "https://github.com/owner/my_crate"

[dependencies]
serde = "1"
`,
		);
		assert.deepStrictEqual(metadata, {
			name: 'my_crate',
			version: '1.2.3',
			description: 'does a thing',
			repository: 'https://github.com/owner/my_crate',
		});
	});

	test('ignores inherited (inline-table and dotted) values', () => {
		const metadata = cargo_toml_parse(
			`[package]
name = "member"
version = { workspace = true }
repository.workspace = true
`,
		);
		// Only the literal string `name` is captured; inherited values have no literal here.
		assert.deepStrictEqual(metadata, {name: 'member'});
	});

	test('strips trailing inline comments and handles single quotes', () => {
		const metadata = cargo_toml_parse(
			`[package]
name = 'quoted' # the crate name
version = "0.4.0"  # current
`,
		);
		assert.deepStrictEqual(metadata, {name: 'quoted', version: '0.4.0'});
	});

	test('first value wins when `[package]` precedes `[workspace.package]`', () => {
		const metadata = cargo_toml_parse(
			`[package]
version = "2.0.0"

[workspace.package]
version = "1.0.0"
`,
		);
		assert.strictEqual(metadata.version, '2.0.0');
	});

	test('returns empty metadata when no identity tables are present', () => {
		assert.deepStrictEqual(cargo_toml_parse('[dependencies]\nserde = "1"\n'), {});
	});
});
