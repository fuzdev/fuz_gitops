/**
 * Reconciles repo metadata across multiple gitops configs.
 *
 * The ecosystem keeps several gitops configs — a canonical superset plus
 * narrower per-project subsets. They can't share one source of truth (the
 * superset includes private repos that public configs may not reference), so the
 * same repo is declared independently in each. This check flags when a subset's
 * intrinsic fields drift from the canonical config, or when a subset lists a
 * repo the canonical config doesn't. `repo_dir` is per-config and ignored.
 *
 * @module
 */

import type {GitopsRepoConfig} from './gitops_config.js';

/** Repo fields that must agree across every config that lists the repo. */
const INTRINSIC_FIELDS = ['visibility', 'ci', 'archived', 'branch'] as const;

export type IntrinsicField = (typeof INTRINSIC_FIELDS)[number];

export type ConfigDriftKind =
	/** The repo is in a subset config but absent from the canonical config. */
	| 'missing_from_canonical'
	/** An intrinsic field disagrees between the subset and the canonical config. */
	| 'field_mismatch';

export interface ConfigDrift {
	repo_url: string;
	/** Name of the subset config whose entry diverges. */
	config: string;
	kind: ConfigDriftKind;
	/** The field that disagrees, set when `kind` is `'field_mismatch'`. */
	field?: IntrinsicField;
	/** The canonical config's value, set when `kind` is `'field_mismatch'`. */
	canonical_value?: string;
	/** The subset config's value, set when `kind` is `'field_mismatch'`. */
	config_value?: string;
}

export interface NamedRepos {
	name: string;
	repos: Array<GitopsRepoConfig>;
}

const field_value = (repo: GitopsRepoConfig, field: IntrinsicField): string => {
	switch (field) {
		case 'visibility':
			return repo.visibility;
		case 'ci':
			return String(repo.ci);
		case 'archived':
			return String(repo.archived);
		case 'branch':
			return repo.branch;
	}
};

/**
 * Compares each subset config against the canonical config. Repos that live only
 * in the canonical config are fine — the subsets are intentional partial views.
 * @returns one `ConfigDrift` per subset entry that is missing from the canonical
 * config or whose intrinsic fields disagree with it
 */
export const reconcile_configs = (
	canonical: NamedRepos,
	subsets: Array<NamedRepos>,
): Array<ConfigDrift> => {
	const drift: Array<ConfigDrift> = [];
	const canonical_by_url = new Map(canonical.repos.map((r) => [r.repo_url, r] as const));
	for (const subset of subsets) {
		for (const repo of subset.repos) {
			const canon = canonical_by_url.get(repo.repo_url);
			if (!canon) {
				drift.push({repo_url: repo.repo_url, config: subset.name, kind: 'missing_from_canonical'});
				continue;
			}
			for (const field of INTRINSIC_FIELDS) {
				const canonical_value = field_value(canon, field);
				const config_value = field_value(repo, field);
				if (canonical_value !== config_value) {
					drift.push({
						repo_url: repo.repo_url,
						config: subset.name,
						kind: 'field_mismatch',
						field,
						canonical_value,
						config_value,
					});
				}
			}
		}
	}
	return drift;
};
