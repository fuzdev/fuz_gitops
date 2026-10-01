/**
 * Type definitions for in-memory repo fixtures used in testing.
 * These fixtures define complete repo state without requiring filesystem or git.
 */

export interface RepoFixtureChangeset {
	/** Changeset filename (e.g. 'breaking.md') */
	filename: string;
	/** Full changeset content including frontmatter */
	content: string;
}

export interface RepoFixtureData {
	/** Short repo name (e.g. 'repo_a') */
	repo_name: string;
	/** Full HTTPS repo URL */
	repo_url: string;
	/** Package.json data */
	package_json: {
		name: string;
		version: string;
		private?: boolean;
		dependencies?: Record<string, string>;
		devDependencies?: Record<string, string>;
		peerDependencies?: Record<string, string>;
	};
	/** Changesets in .changeset/ directory */
	changesets?: Array<RepoFixtureChangeset>;
}

export interface RepoFixtureExpectedVersionChange {
	package_name: string;
	from: string;
	to: string;
	scenario: 'explicit_changeset' | 'bump_escalation' | 'auto_generated';
}

export interface RepoFixtureSet {
	/** Unique name for this fixture set */
	name: string;
	/** Human-readable description of what this fixture tests */
	description: string;
	/** Array of repo data */
	repos: Array<RepoFixtureData>;
	/** Expected outcomes when running publishing operations */
	expected_outcomes: {
		/** Expected topological order for publishing */
		publishing_order: Array<string>;
		/** Expected version changes */
		version_changes: Array<RepoFixtureExpectedVersionChange>;
		/** Expected breaking change cascades (source package -> affected packages) */
		breaking_cascades?: Record<string, Array<string>>;
		/** Expected informational sentences (excluded repos, dev cycles) */
		info?: Array<string>;
		/** Expected packages with nothing to publish — omitted means none */
		no_changes?: Array<string>;
		/** Expected warnings */
		warnings?: Array<string>;
		/** Expected errors */
		errors?: Array<string>;
	};
}
