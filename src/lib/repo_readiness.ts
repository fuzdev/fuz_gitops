/**
 * Whether each configured repo sits where the registry puts it, read from the
 * entries `repos status --json` reports: the facts `gitops_publish --wetrun`
 * gates on, and the at-rest block the read-only diagnostics print.
 *
 * Pure — a report's facts in, problems and their messages out; no git calls.
 * The facts are decided on the Rust side (`at_rest`, `fetch_error`, a
 * checkout's `busy`, `needs_human`); this reads them, never re-derives them.
 *
 * Two readings:
 *
 * - **at rest** (`repo_readiness_at_rest`) — the primary checkout on the
 *   branch the entry follows, clean (untracked files count), no operation in
 *   progress, and that branch in sync with its origin upstream as of the
 *   local remote-tracking refs. What the diagnostics report.
 * - **ready to publish** (`repo_readiness_for_publish`) — at rest, except
 *   that the followed branch may be ahead of origin (origin's tip is then an
 *   ancestor, so the plan misses nothing and the release push is still a
 *   fast-forward), and also fetched without error (so the relation is
 *   origin's current word), no other live Claude Code session in the primary
 *   checkout (the executor commits there), and nothing the entry leaves to a
 *   person (`needs_human`).
 *
 * @module
 */

import type { Result } from '@fuzdev/fuz_util/result.ts';

import type {
	ReposEntryStatus,
	ReposFetchFailure,
	ReposHead,
	ReposInProgressOp,
	ReposNeedsHuman,
	ReposRelation,
	ReposSession,
	ReposStatusReport,
	ReposUncommitted,
	ReposUnavailable
} from './repos_status.ts';

/** One way a repo isn't where the registry puts it, or isn't ready to publish. */
export type RepoReadinessProblem =
	| { kind: 'unprobed'; detail: string }
	| { kind: 'no_branch' }
	| { kind: 'off_branch'; branch: string; head: ReposHead }
	| { kind: 'dirty'; uncommitted: ReposUncommitted }
	| { kind: 'in_progress'; op: ReposInProgressOp | null }
	| { kind: 'followed'; branch: string; relation: ReposRelation | null }
	| { kind: 'fetch_failed'; failure: ReposFetchFailure }
	| { kind: 'busy'; sessions: Array<ReposSession> }
	| { kind: 'needs_human'; reason: ReposNeedsHuman };

/** A repo's readiness problems, by registry key; `entry` is `null` when the report lacks it. */
export interface RepoReadiness {
	key: string;
	entry: ReposEntryStatus | null;
	problems: Array<RepoReadinessProblem>;
}

/**
 * The ways an entry's primary checkout isn't at rest where the registry puts
 * it, from its `at_rest` facts: empty when it is.
 *
 * @param entry - the entry as `repos status --json` reported it
 * @returns the problems, in a fixed order: unprobed alone, else branch, dirt, operation, then the followed branch's relation
 */
export const repo_readiness_at_rest = (entry: ReposEntryStatus): Array<RepoReadinessProblem> => {
	const { at_rest, branch } = entry;
	const primary = entry.checkouts[0];
	if (at_rest === null || primary === undefined) {
		return [{ kind: 'unprobed', detail: unprobed_detail(entry) }];
	}
	const problems: Array<RepoReadinessProblem> = [];
	if (branch === null || at_rest.on_branch === null) {
		problems.push({ kind: 'no_branch' });
	} else if (!at_rest.on_branch) {
		problems.push({ kind: 'off_branch', branch, head: primary.head });
	}
	if (!at_rest.clean) problems.push({ kind: 'dirty', uncommitted: primary.uncommitted });
	if (!at_rest.idle) problems.push({ kind: 'in_progress', op: primary.in_progress });
	if (branch !== null && at_rest.followed?.kind !== 'in_sync') {
		problems.push({ kind: 'followed', branch, relation: at_rest.followed });
	}
	return problems;
};

/**
 * The ways an entry isn't ready for `gitops_publish --wetrun`: not at rest
 * (`repo_readiness_at_rest`) other than its followed branch being ahead of
 * origin, its fetch failed, another live session works in its primary
 * checkout, or it has a `needs_human` reason. Ahead is ready: origin's tip is
 * an ancestor of the branch, so the plan misses nothing and `gro publish`'s
 * push stays a fast-forward, pushing those commits with the release (see
 * `format_readiness_ahead`). A reason restating
 * an at-rest problem (the primary's operation or detached HEAD) is left out,
 * and a `default_branch_*` reason stands in for the followed-branch problem
 * it explains.
 *
 * @param entry - the entry as `repos status --fetch --json` reported it
 * @returns the problems; empty when the entry is ready
 */
export const repo_readiness_for_publish = (
	entry: ReposEntryStatus
): Array<RepoReadinessProblem> => {
	let problems = repo_readiness_at_rest(entry).filter(
		(p) => !(p.kind === 'followed' && p.relation?.kind === 'ahead')
	);
	if (entry.fetch_error !== null) {
		problems.push({ kind: 'fetch_failed', failure: entry.fetch_error });
	}
	const primary = entry.checkouts[0];
	if (primary && primary.busy.length > 0) {
		problems.push({ kind: 'busy', sessions: primary.busy });
	}
	const has = (kind: RepoReadinessProblem['kind']): boolean =>
		problems.some((p) => p.kind === kind);
	const reasons = entry.needs_human.filter((reason) => {
		switch (reason.kind) {
			case 'operation_in_progress':
				return !(reason.checkout === primary?.path && has('in_progress'));
			case 'unexpected_detached':
				return !(reason.checkout === primary?.path && has('off_branch'));
			default:
				return true;
		}
	});
	if (
		reasons.some(
			(r) =>
				r.kind === 'default_branch_missing' ||
				r.kind === 'default_branch_no_upstream' ||
				r.kind === 'default_branch_gone'
		)
	) {
		problems = problems.filter((p) => p.kind !== 'followed');
	}
	for (const reason of reasons) problems.push({ kind: 'needs_human', reason });
	return problems;
};

/** Options for the messages naming what's wrong and the fix. */
export interface RepoReadinessFormatOptions {
	/** The `repos` invocation fixes name; `repos --registry <path>` when the run passed one. */
	repos_command?: string;
}

/**
 * What a readiness problem says is wrong with the repo keyed `key`, and the
 * fix, when there is one to name.
 */
export const format_repo_readiness_problem = (
	key: string,
	problem: RepoReadinessProblem,
	options: RepoReadinessFormatOptions = {}
): { what: string; fix: string } => {
	const repos = options.repos_command ?? 'repos';
	const status_fix = `\`${repos} status ${key}\` says what it found`;
	switch (problem.kind) {
		case 'unprobed':
			return { what: `can't be read: ${problem.detail}`, fix: status_fix };
		case 'no_branch':
			return {
				what: 'its registry entry follows no branch',
				fix: 'declare its `branch` in repos.toml'
			};
		case 'off_branch': {
			const { branch, head } = problem;
			return {
				what:
					head.kind === 'branch'
						? `on \`${head.name}\`, not \`${branch}\``
						: `detached at ${head.commit.slice(0, 12)}, not on \`${branch}\``,
				fix: `switch to \`${branch}\` once the work there is committed or stashed`
			};
		}
		case 'dirty':
			return {
				what: `uncommitted changes (${format_uncommitted(problem.uncommitted)})`,
				fix: 'commit, stash, or discard them (untracked files count) — after a failed publish, see the troubleshooting doc first'
			};
		case 'in_progress':
			return {
				what: `${problem.op === null ? 'an operation' : `a ${OP_LABELS[problem.op]}`} is in progress`,
				fix: problem.op === null ? 'finish or abort it' : OP_FIXES[problem.op]
			};
		case 'followed':
			return format_followed(key, problem.branch, problem.relation, repos);
		case 'fetch_failed':
			return {
				what: `fetching origin failed (${format_fetch_failure(problem.failure)})`,
				fix: `fix the remote, then rerun (\`${repos} status --fetch ${key}\` retries the fetch)`
			};
		case 'busy':
			return {
				what: `another live Claude Code session works in its checkout (pid ${problem.sessions.map((s) => s.pid).join(', ')})`,
				fix: 'wait for it to finish, or end it'
			};
		case 'needs_human':
			return {
				what: format_needs_human(problem.reason),
				fix: `\`${repos} status ${key}\` names the fix`
			};
	}
};

/**
 * Checks a `repos status --fetch --json` report on the repos a real publish
 * reads and writes: every key's entry ready (`repo_readiness_for_publish`),
 * the report fetched, and busy detection available, since a live session it
 * can't vouch for may be working in any checkout.
 *
 * @param options.report - the report, made with `--fetch`
 * @param options.keys - the registry keys that must be ready
 * @returns `ahead`, the ready repos whose followed branch is ahead of origin; and on failure a message naming each repo, what's wrong, and the fix, its `lines`, and every not-ready repo
 */
export const check_publish_readiness = (
	options: {
		report: ReposStatusReport;
		keys: ReadonlyArray<string>;
	} & RepoReadinessFormatOptions
): Result<
	{ ahead: Array<ReadinessAhead> },
	{
		message: string;
		lines: Array<string>;
		not_ready: Array<RepoReadiness>;
		ahead: Array<ReadinessAhead>;
	}
> => {
	const { report, keys, repos_command } = options;
	const repos = repos_command ?? 'repos';
	const by_key = new Map(report.entries.map((e) => [e.key, e] as const));

	const lines: Array<string> = [];
	if (!report.fetched) {
		lines.push(`the report wasn't fetched, so "in sync" is only as fresh as the last fetch`);
	}
	if (report.sessions.kind === 'unavailable') {
		lines.push(
			`busy detection is unavailable (${format_unavailable(report.sessions.reason)}), so a live session can't be ruled out of any checkout — \`${repos} status\` says why`
		);
	}

	const not_ready: Array<RepoReadiness> = [];
	const ahead: Array<ReadinessAhead> = [];
	for (const key of keys) {
		const entry = by_key.get(key) ?? null;
		const problems: Array<RepoReadinessProblem> = entry
			? repo_readiness_for_publish(entry)
			: [{ kind: 'unprobed', detail: 'not in the `repos status` report' }];
		if (problems.length === 0) {
			const followed = entry?.at_rest?.followed;
			if (entry?.branch && followed?.kind === 'ahead') {
				ahead.push({ key, branch: entry.branch, commits: followed.commits });
			}
			continue;
		}
		not_ready.push({ key, entry, problems });
		for (const problem of problems) {
			const { what, fix } = format_repo_readiness_problem(key, problem, options);
			lines.push(`${key}: ${what} — ${fix}`);
		}
	}

	if (lines.length === 0) return { ok: true, ahead };
	const message =
		'not publishing, and nothing was changed: the plan reads every npm repo as it sits, ' +
		`and publishing commits and pushes there, so each must be ready:\n  ${lines.join('\n  ')}`;
	return { ok: false, message, lines, not_ready, ahead };
};

/** A ready repo whose followed branch is ahead of origin: commits a publish pushes. */
export interface ReadinessAhead {
	key: string;
	branch: string;
	commits: number;
}

/**
 * Says what happens to a ready repo's commits ahead of origin: its release
 * push carries them when it publishes, else they stay unpushed.
 *
 * @param ahead - the repo, its branch, and how many commits it's ahead
 * @param publishes - whether the plan publishes it
 */
export const format_readiness_ahead = (ahead: ReadinessAhead, publishes: boolean): string => {
	const head = `${ahead.key}: \`${ahead.branch}\` is ${plural(ahead.commits, 'commit')} ahead of origin`;
	return publishes
		? `${head} — publishing pushes them with the release`
		: `${head} — it doesn't publish, so they stay unpushed until \`repos sync\` or \`repos push\``;
};

/**
 * The repos not at rest, for the read-only diagnostics' readiness block.
 *
 * @param entries - the entries of the repos a diagnostic reads
 * @returns each entry with at-rest problems, in the order given
 */
export const repos_not_at_rest = (entries: ReadonlyArray<ReposEntryStatus>): Array<RepoReadiness> =>
	entries
		.map((entry) => ({ key: entry.key, entry, problems: repo_readiness_at_rest(entry) }))
		.filter((r) => r.problems.length > 0);

/**
 * Formats the diagnostics' readiness block: a header and a line per problem,
 * saying what each repo not at rest is doing instead — or no lines when every
 * repo is at rest. Not a failure: the diagnostics read repos as they sit.
 *
 * @param not_ready - `repos_not_at_rest`'s result
 * @param now - the current time in unix seconds, for how long ago each repo was fetched
 * @returns the block's lines, unindented header first
 */
export const format_readiness_block = (
	not_ready: ReadonlyArray<RepoReadiness>,
	now: number
): Array<string> => {
	if (not_ready.length === 0) return [];
	const lines = [
		'not at rest, so read as they sit (a real publish refuses all but a branch ahead of origin):'
	];
	for (const { key, entry, problems } of not_ready) {
		for (const problem of problems) {
			const { what } = format_repo_readiness_problem(key, problem);
			// relations are as of the remote-tracking refs, so say how old they are
			const as_of =
				problem.kind === 'followed' && entry
					? ` (${entry.fetched_at === null ? 'never fetched' : `fetched ${format_age(now - entry.fetched_at)} ago`})`
					: '';
			lines.push(`  ${key}: ${what}${as_of}`);
		}
	}
	return lines;
};

const OP_LABELS: Record<ReposInProgressOp, string> = {
	rebase: 'rebase',
	merge: 'merge',
	cherry_pick: 'cherry-pick',
	revert: 'revert',
	bisect: 'bisect',
	sequencer: 'cherry-pick or revert sequence',
	am: '`git am`'
};

const OP_FIXES: Record<ReposInProgressOp, string> = {
	rebase: 'finish or abort it (`git rebase --continue` or `--abort`)',
	merge: 'finish or abort it (`git merge --continue` or `--abort`)',
	cherry_pick: 'finish or abort it (`git cherry-pick --continue` or `--abort`)',
	revert: 'finish or abort it (`git revert --continue` or `--abort`)',
	bisect: 'end it (`git bisect reset`)',
	sequencer: 'finish or abort it (`git cherry-pick` or `git revert`, `--continue` or `--abort`)',
	am: 'finish or abort it (`git am --continue` or `--abort`)'
};

const format_followed = (
	key: string,
	branch: string,
	relation: ReposRelation | null,
	repos: string
): { what: string; fix: string } => {
	const b = `\`${branch}\``;
	if (relation === null) {
		return {
			what: `${b} isn't compared with origin`,
			fix: `\`${repos} status ${key}\` says why`
		};
	}
	switch (relation.kind) {
		case 'in_sync':
			return { what: `${b} is in sync with origin`, fix: 'nothing' };
		case 'ahead':
			return {
				what: `${b} is ${plural(relation.commits, 'commit')} ahead of origin (unpushed)`,
				fix: `\`${repos} sync ${key}\` pushes it`
			};
		case 'behind':
			return {
				what: `${b} is ${plural(relation.commits, 'commit')} behind origin`,
				fix: `\`${repos} sync ${key}\` fast-forwards it`
			};
		case 'diverged':
			return {
				what: `${b} has diverged from origin (${relation.ahead} ahead, ${relation.behind} behind)`,
				fix: 'rebase or merge it by hand, then push'
			};
		case 'shallow':
			return {
				what: `${b} is shallow and not at origin's tip`,
				fix: `\`${repos} sync ${key}\` moves it`
			};
		case 'gone':
			return {
				what: `${b} tracks an upstream gone from origin`,
				fix: 'repoint its upstream by hand'
			};
		case 'unmapped':
			return {
				what: `${b} tracks an upstream origin's fetch refspec doesn't map`,
				fix: "fix origin's fetch refspec by hand"
			};
		case 'untracked':
			return {
				what: `${b} tracks no upstream on origin`,
				fix: `set it (\`git branch -u origin/${branch} ${branch}\`)`
			};
	}
};

const format_needs_human = (reason: ReposNeedsHuman): string => {
	switch (reason.kind) {
		case 'not_a_repo':
			return `isn't a repo: ${reason.detail}`;
		case 'operation_in_progress':
			return `a ${OP_LABELS[reason.op]} is in progress in ${reason.checkout}`;
		case 'origin_mismatch':
			return `origin isn't the registry's repo (${reason.expected})`;
		case 'origin_not_https':
			return `origin isn't the registry's repo over HTTPS (${reason.expected})`;
		case 'fetch_url_mismatch':
			return `origin's fetch reaches ${reason.fetch_url}, not ${reason.expected}`;
		case 'worktree_unreadable':
			return `a worktree's git dir can't be read (${reason.path})`;
		case 'default_branch_missing':
			return `has no local \`${reason.branch}\`, the branch its entry follows`;
		case 'default_branch_no_upstream':
			return `\`${reason.branch}\` tracks no upstream on origin`;
		case 'default_branch_gone':
			return `\`${reason.branch}\`'s upstream is gone from origin`;
		case 'unexpected_detached':
			return `${reason.checkout} is detached`;
		case 'checkout_unresolvable':
			return `a checkout's path can't be resolved (${reason.path}: ${reason.error}), so a session there can't be ruled out`;
		case 'unlisted_git_dir':
			return `a git dir no worktree list names shares its refs (${reason.git_dir})`;
		case 'push_url_mismatch':
			return `origin's push URL isn't the registry's repo (${reason.expected})`;
		case 'clone_shares_repo':
			return `shares its repo with \`${reason.with}\``;
		case 'cloned_unregistered':
			return `an unregistered dir clones it (${reason.dir})`;
	}
};

const format_fetch_failure = (failure: ReposFetchFailure): string => {
	switch (failure.kind) {
		case 'ref_gone':
			return `ref_gone: ${failure.refname}`;
		case 'unreachable':
			return `unreachable (${failure.cause}): ${failure.message}`;
		case 'repo_not_found':
			return `repo_not_found: ${failure.message}`;
		case 'timed_out':
			return `timed out after ${failure.after_secs}s`;
		case 'failed':
			return failure.message;
		case 'refspec_outside_origin':
			return `refused: refspec ${failure.refspec} writes outside refs/remotes/origin/`;
		case 'origin_refs_shared':
			return `refused: remote ${failure.remote}'s refspec ${failure.refspec} writes origin's refs`;
		case 'legacy_remotes_unreadable':
			return `refused: ${failure.path} can't be read`;
	}
};

const format_unavailable = (reason: ReposUnavailable): string => {
	switch (reason.kind) {
		case 'home_unknown':
			return 'HOME is unset';
		case 'relative_config_dir':
			return `a relative CLAUDE_CONFIG_DIR, ${reason.path}`;
		case 'unreadable':
		case 'unparseable':
			return `${reason.kind}: ${reason.path}`;
		case 'foreign_pid_domain':
			return `a session from another machine or pid namespace: ${reason.path}`;
	}
};

const unprobed_detail = (entry: ReposEntryStatus): string => {
	if (entry.probe_error !== null) return `probing failed: ${entry.probe_error}`;
	switch (entry.presence.kind) {
		case 'missing':
			return 'missing';
		case 'not_a_repo':
			return 'not a git repo';
		case 'present':
			return 'no checkout was probed';
	}
};

const format_uncommitted = (u: ReposUncommitted): string =>
	(
		[
			['staged', u.staged],
			['unstaged', u.unstaged],
			['untracked', u.untracked],
			['conflicted', u.conflicted]
		] as const
	)
		.filter(([, n]) => n > 0)
		.map(([label, n]) => `${n} ${label}`)
		.join(', ');

const plural = (n: number, noun: string): string => `${n} ${noun}${n === 1 ? '' : 's'}`;

const format_age = (seconds: number): string => {
	const s = Math.max(0, seconds);
	if (s < 60) return 'under a minute';
	if (s < 3600) return `${Math.floor(s / 60)}m`;
	if (s < 86400) return `${Math.floor(s / 3600)}h`;
	return `${Math.floor(s / 86400)}d`;
};
