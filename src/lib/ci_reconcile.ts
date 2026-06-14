/**
 * Reconciles each repo's declared `ci` flag against whether it actually has
 * GitHub Actions workflow files on disk.
 *
 * The gitops config derives `ci` from visibility (on for public repos, off for
 * private) with per-repo overrides; this check catches drift between that
 * declaration and reality —
 * a repo that claims CI but ships no workflow, or one that disclaims CI yet
 * still carries one. Repos that aren't checked out locally can't be judged, so
 * the caller marks them uncheckable and they're skipped. Archived repos are
 * frozen on their host, so their CI state is intentionally left alone and they're
 * skipped too.
 *
 * @module
 */

import {existsSync, readdirSync} from 'node:fs';
import {join} from 'node:path';

/** How a repo's declared `ci` diverges from its workflow files on disk. */
export type CiDriftKind =
	/** `ci` is `true` but the repo has no workflow files. */
	| 'missing_ci'
	/** `ci` is `false` but the repo has workflow files. */
	| 'stray_ci';

export interface CiDrift {
	repo_url: string;
	/** The declared/derived `ci` value. */
	ci: boolean;
	has_workflows: boolean;
	kind: CiDriftKind;
}

export interface CiReconcileInput {
	repo_url: string;
	/** The declared/derived `ci` value from the gitops config. */
	ci: boolean;
	/** Whether the repo has at least one workflow file on disk. */
	has_workflows: boolean;
	/** Whether the repo is checked out locally; uncheckable repos are skipped. */
	checkable: boolean;
	/** Whether the repo is archived (frozen) on its host; archived repos are skipped. */
	archived: boolean;
}

/**
 * Compares each repo's declared `ci` against its actual workflow presence.
 * @returns one `CiDrift` per repo whose declaration and reality disagree
 */
export const reconcile_ci = (repos: Array<CiReconcileInput>): Array<CiDrift> => {
	const drift: Array<CiDrift> = [];
	for (const repo of repos) {
		if (!repo.checkable || repo.archived) continue;
		const {repo_url, ci, has_workflows} = repo;
		if (ci && !has_workflows) {
			drift.push({repo_url, ci, has_workflows, kind: 'missing_ci'});
		} else if (!ci && has_workflows) {
			drift.push({repo_url, ci, has_workflows, kind: 'stray_ci'});
		}
	}
	return drift;
};

/**
 * Whether a local repo directory contains at least one GitHub Actions workflow.
 * @param repo_dir - absolute or cwd-relative path to the repo's local directory
 */
export const repo_has_workflows = (repo_dir: string): boolean => {
	const workflows_dir = join(repo_dir, '.github', 'workflows');
	if (!existsSync(workflows_dir)) return false;
	return readdirSync(workflows_dir).some((file) => file.endsWith('.yml') || file.endsWith('.yaml'));
};
