/**
 * Zod schemas for the `repos status --json` document, the report the Rust
 * `repos` tool (`crates/fuz_repos`) prints, and the error document it prints
 * in place of the report on a fatal error.
 *
 * The schemas mirror the Rust types by hand, each named as its Rust type
 * prefixed `Repos` (`EntryStatus` is `ReposEntryStatus`), with fields as
 * serde writes them: snake_case, a tagged enum as an object whose `kind`
 * names its variant, an `Option` as `null` (never omitted), counts as
 * integers, and times as unix seconds. A flattened enum's `kind`
 * and payload sit beside its struct's own fields, so those structs are
 * unions here (`ReposUnregisteredClone`, `ReposStatusErrorBody`). Where a
 * Rust enum holds variants the status document never carries, the mirror
 * leaves them out: `ReposFetchFailure` has no push rejection, and
 * `ReposStatusErrorBody` holds only the errors `repos status --json` can
 * print.
 *
 * Every object is strict and every union closed, so a field or variant the
 * mirror doesn't know fails the parse. The Rust side bumps `version` on any
 * change to the document's shape; a consumer checks it first, and on a
 * mismatch reports both versions and how to install the binary this repo
 * builds (`cargo install --path crates/fuz_repos --locked`) rather than a
 * parse error.
 *
 * Drift is caught by the golden fixtures: a Rust test writes every status
 * document shape to `src/test/fixtures/repos_status/`, covering every
 * variant the status report and its error document can hold, and a TS test
 * parses each with these schemas. The field semantics live in the Rust
 * rustdoc and the JSON contract in `docs/repos.md`. The `repos sync --json`
 * and `repos push --json` documents have no TS consumer yet and aren't
 * mirrored here.
 *
 * @module
 */

import { z } from 'zod';

/**
 * The `repos status --json` document's format version, Rust's
 * `STATUS_FORMAT_VERSION`: the one these schemas parse.
 */
export const REPOS_STATUS_FORMAT_VERSION = 17;

// u32 and u64 on the Rust side
const Count = z.number().int().nonnegative();
const UnixSeconds = z.number().int().nonnegative().meta({ description: 'a time in unix seconds' });

// --- sessions ---

/** Where a live Claude Code session was recorded. */
export const ReposSessionSource = z.enum(['session_file', 'roster_worker']);
export type ReposSessionSource = z.infer<typeof ReposSessionSource>;

/** A live Claude Code session: its process, and where it works. */
export const ReposSession = z.strictObject({
	pid: Count,
	cwd: z.string(),
	worktree: z.string().nullable(),
	process_cwd: z.string().nullable(),
	source: ReposSessionSource
});
export type ReposSession = z.infer<typeof ReposSession>;

/** Why busy detection can't vouch for every live session. */
export const ReposUnavailable = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('home_unknown') }),
	z.strictObject({ kind: z.literal('relative_config_dir'), path: z.string() }),
	z.strictObject({ kind: z.literal('unreadable'), path: z.string(), error: z.string() }),
	z.strictObject({ kind: z.literal('unparseable'), path: z.string(), error: z.string() }),
	z.strictObject({
		kind: z.literal('foreign_pid_domain'),
		path: z.string(),
		pid_domain: z.string(),
		source: ReposSessionSource
	})
]);
export type ReposUnavailable = z.infer<typeof ReposUnavailable>;

/** Busy detection as the report carries it. */
export const ReposSessions = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('available'), unscoped: z.array(ReposSession) }),
	z.strictObject({ kind: z.literal('unavailable'), reason: ReposUnavailable })
]);
export type ReposSessions = z.infer<typeof ReposSessions>;

// --- registry ---

/** Which registry table an entry comes from. */
export const ReposEntryKind = z.enum(['repo', 'reference']);
export type ReposEntryKind = z.infer<typeof ReposEntryKind>;

/** A repo's declared visibility on its host. */
export const ReposVisibility = z.enum(['public', 'private']);
export type ReposVisibility = z.infer<typeof ReposVisibility>;

/** A repo's list of the sibling checkouts it uses. */
export const ReposCheckoutList = z.enum(['requires', 'consults']);
export type ReposCheckoutList = z.infer<typeof ReposCheckoutList>;

/** An entry by table and key. */
export const ReposEntryName = z.strictObject({ kind: ReposEntryKind, key: z.string() });
export type ReposEntryName = z.infer<typeof ReposEntryName>;

/** A registry integrity rule broken. */
export const ReposRegistryIssue = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('repo_not_owned'), key: z.string(), account: z.string() }),
	z.strictObject({ kind: z.literal('fork_not_owned'), key: z.string() }),
	z.strictObject({ kind: z.literal('dir_not_a_name'), entry: ReposEntryName, dir: z.string() }),
	z.strictObject({
		kind: z.literal('dir_claimed_twice'),
		dir: z.string(),
		first: ReposEntryName,
		second: ReposEntryName
	}),
	z.strictObject({ kind: z.literal('key_in_both'), key: z.string() }),
	z.strictObject({ kind: z.literal('key_is_other_dir'), key: z.string(), entry: ReposEntryName }),
	z.strictObject({
		kind: z.literal('unknown_checkout_ref'),
		key: z.string(),
		field: ReposCheckoutList,
		target: z.string()
	}),
	z.strictObject({ kind: z.literal('self_ref'), key: z.string(), field: ReposCheckoutList }),
	z.strictObject({ kind: z.literal('requires_and_consults'), key: z.string(), target: z.string() })
]);
export type ReposRegistryIssue = z.infer<typeof ReposRegistryIssue>;

// --- verdicts ---

/** What holds an action back. */
export const ReposHeldBy = z.enum([
	'pinned',
	'entry',
	'push_url',
	'origin_not_https',
	'fetch_failed',
	'dirty_checkout',
	'unprobed_worktree',
	'several_checkouts',
	'busy',
	'busy_unknown'
]);
export type ReposHeldBy = z.infer<typeof ReposHeldBy>;

/** What a run does about refreshing a third-party reference or a pin. */
export const ReposRefreshVerdict = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('act') }),
	z.strictObject({ kind: z.literal('held'), by: ReposHeldBy })
]);
export type ReposRefreshVerdict = z.infer<typeof ReposRefreshVerdict>;

/** Whether a repo is at the entry's dir. */
export const ReposPresence = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('present') }),
	z.strictObject({ kind: z.literal('missing') }),
	z.strictObject({ kind: z.literal('not_a_repo') })
]);
export type ReposPresence = z.infer<typeof ReposPresence>;

/** How sync would clone a missing entry. */
export const ReposCloneRecipe = z.strictObject({
	url: z.string(),
	branch: z.string().nullable(),
	shallow: z.boolean(),
	sparse: z.string().nullable()
});
export type ReposCloneRecipe = z.infer<typeof ReposCloneRecipe>;

/** What sync does about a missing entry: clone it, or why not yet. */
export const ReposCloneVerdict = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('act'), recipe: ReposCloneRecipe }),
	z.strictObject({ kind: z.literal('held'), recipe: ReposCloneRecipe, by: ReposHeldBy })
]);
export type ReposCloneVerdict = z.infer<typeof ReposCloneVerdict>;

/** A sync action on a branch. */
export const ReposSyncAction = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('push'), commits: Count }),
	z.strictObject({ kind: z.literal('fast_forward'), commits: Count }),
	z.strictObject({ kind: z.literal('move') })
]);
export type ReposSyncAction = z.infer<typeof ReposSyncAction>;

/** Why a branch is left to a person. */
export const ReposBranchNeedsHuman = z.enum([
	'diverged',
	'unmapped',
	'archived_ahead',
	'shallow_local_work',
	'upstream_not_a_branch'
]);
export type ReposBranchNeedsHuman = z.infer<typeof ReposBranchNeedsHuman>;

/** Why a branch reads as cleanup. */
export const ReposCleanupReason = z.enum(['merged', 'upstream_gone']);
export type ReposCleanupReason = z.infer<typeof ReposCleanupReason>;

/** What sync does about a branch. */
export const ReposVerdict = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('quiet') }),
	z.strictObject({ kind: z.literal('act'), action: ReposSyncAction }),
	z.strictObject({ kind: z.literal('held'), action: ReposSyncAction, by: ReposHeldBy }),
	z.strictObject({ kind: z.literal('needs_human'), reason: ReposBranchNeedsHuman }),
	z.strictObject({ kind: z.literal('local_only') }),
	z.strictObject({
		kind: z.literal('cleanup'),
		reason: ReposCleanupReason,
		removable_worktree: z.string().nullable()
	})
]);
export type ReposVerdict = z.infer<typeof ReposVerdict>;

// --- checkouts and branches ---

/** A clone's shape: shallow, sparse, or partial. */
export const ReposLayout = z.strictObject({
	shallow: z.boolean(),
	sparse: z.boolean(),
	partial_filter: z.string().nullable()
});
export type ReposLayout = z.infer<typeof ReposLayout>;

/** A checkout's HEAD. */
export const ReposHead = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('branch'), name: z.string() }),
	z.strictObject({ kind: z.literal('detached'), commit: z.string() })
]);
export type ReposHead = z.infer<typeof ReposHead>;

/** A checkout's uncommitted work, by kind. */
export const ReposUncommitted = z.strictObject({
	staged: Count,
	unstaged: Count,
	untracked: Count,
	conflicted: Count
});
export type ReposUncommitted = z.infer<typeof ReposUncommitted>;

/** A git operation in progress. */
export const ReposInProgressOp = z.enum([
	'rebase',
	'merge',
	'cherry_pick',
	'revert',
	'bisect',
	'sequencer',
	'am'
]);
export type ReposInProgressOp = z.infer<typeof ReposInProgressOp>;

/** One checkout of a repo: its primary, or a linked worktree probed. */
export const ReposCheckout = z.strictObject({
	path: z.string(),
	primary: z.boolean(),
	head: ReposHead,
	uncommitted: ReposUncommitted,
	in_progress: ReposInProgressOp.nullable(),
	locked: z.boolean(),
	linked: z.boolean(),
	submodules: z.boolean().nullable(),
	busy: z.array(ReposSession)
});
export type ReposCheckout = z.infer<typeof ReposCheckout>;

/** A branch's relation to its upstream. */
export const ReposRelation = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('in_sync') }),
	z.strictObject({ kind: z.literal('ahead'), commits: Count }),
	z.strictObject({ kind: z.literal('behind'), commits: Count }),
	z.strictObject({ kind: z.literal('diverged'), ahead: Count, behind: Count }),
	z.strictObject({ kind: z.literal('shallow') }),
	z.strictObject({ kind: z.literal('gone') }),
	z.strictObject({ kind: z.literal('unmapped') }),
	z.strictObject({ kind: z.literal('untracked') })
]);
export type ReposRelation = z.infer<typeof ReposRelation>;

/** One local branch and its verdict. */
export const ReposBranchStatus = z.strictObject({
	name: z.string(),
	upstream: z.string().nullable(),
	worktree: z.string().nullable(),
	symref: z.string().nullable(),
	unique_commits: Count,
	newest_commit_at: UnixSeconds,
	relation: ReposRelation,
	verdict: ReposVerdict
});
export type ReposBranchStatus = z.infer<typeof ReposBranchStatus>;

/** Whether an entry's primary checkout sits where the registry puts it. */
export const ReposAtRest = z.strictObject({
	on_branch: z.boolean().nullable(),
	clean: z.boolean(),
	idle: z.boolean(),
	followed: ReposRelation.nullable()
});
export type ReposAtRest = z.infer<typeof ReposAtRest>;

// --- worktrees not probed ---

/** An unprobed worktree's HEAD, as its git dir records it. */
export const ReposUnprobedHead = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('branch'), name: z.string() }),
	z.strictObject({ kind: z.literal('detached'), commit: z.string() }),
	z.strictObject({ kind: z.literal('unknown') })
]);
export type ReposUnprobedHead = z.infer<typeof ReposUnprobedHead>;

/** Why a worktree wasn't probed. */
export const ReposUnprobedWhy = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('prunable') }),
	z.strictObject({ kind: z.literal('missing') }),
	z.strictObject({ kind: z.literal('failed'), error: z.string() })
]);
export type ReposUnprobedWhy = z.infer<typeof ReposUnprobedWhy>;

/** What an unprobed worktree's git dir holds that a prune would lose. */
export const ReposGitDirHolds = z.strictObject({
	submodules: z.boolean(),
	worktree_refs: z.boolean(),
	staged: z.boolean().nullable()
});
export type ReposGitDirHolds = z.infer<typeof ReposGitDirHolds>;

/** What pruning an unprobed worktree would lose. */
export const ReposPruneLoss = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('operation'), op: ReposInProgressOp }),
	z.strictObject({ kind: z.literal('detached_head') }),
	z.strictObject({ kind: z.literal('unknown_head') }),
	z.strictObject({ kind: z.literal('missing_branch'), name: z.string() }),
	z.strictObject({ kind: z.literal('submodules') }),
	z.strictObject({ kind: z.literal('worktree_refs') }),
	z.strictObject({ kind: z.literal('staged_changes') }),
	z.strictObject({ kind: z.literal('unmatched_git_dir') }),
	z.strictObject({ kind: z.literal('relative_gitdir'), git_dir: z.string() })
]);
export type ReposPruneLoss = z.infer<typeof ReposPruneLoss>;

/** Whether pruning an unprobed worktree is safe. */
export const ReposPrune = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('safe') }),
	z.strictObject({ kind: z.literal('loses'), losses: z.array(ReposPruneLoss) }),
	z.strictObject({ kind: z.literal('moved'), to: z.array(z.string()) })
]);
export type ReposPrune = z.infer<typeof ReposPrune>;

/** A worktree of the repo that couldn't be probed. */
export const ReposUnprobedWorktree = z.strictObject({
	path: z.string(),
	git_dir: z.string().nullable(),
	head: ReposUnprobedHead,
	locked: z.boolean(),
	in_progress: ReposInProgressOp.nullable(),
	why: ReposUnprobedWhy,
	holds: ReposGitDirHolds.nullable()
});
export type ReposUnprobedWorktree = z.infer<typeof ReposUnprobedWorktree>;

/** An unprobed worktree (flattened) with its prune verdict and live sessions. */
export const ReposUnprobedWorktreeStatus = ReposUnprobedWorktree.extend({
	prune: ReposPrune.nullable(),
	busy: z.array(ReposSession)
});
export type ReposUnprobedWorktreeStatus = z.infer<typeof ReposUnprobedWorktreeStatus>;

// --- remotes ---

/** The `origin` remote as git sees it, when it isn't the registry's `url`. */
export const ReposOriginRemote = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('url'), url: z.string() }),
	z.strictObject({ kind: z.literal('no_url') }),
	z.strictObject({ kind: z.literal('missing') })
]);
export type ReposOriginRemote = z.infer<typeof ReposOriginRemote>;

/** Why no `git remote` command fits an origin's fix. */
export const ReposOriginByHand = z.enum([
	'outside_repo_file',
	'valueless_url',
	'empty_value',
	'several_urls'
]);
export type ReposOriginByHand = z.infer<typeof ReposOriginByHand>;

/** How to point `origin` at the registry's URL. */
export const ReposOriginFix = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('add') }),
	z.strictObject({ kind: z.literal('set_url') }),
	z.strictObject({ kind: z.literal('by_hand'), reason: ReposOriginByHand })
]);
export type ReposOriginFix = z.infer<typeof ReposOriginFix>;

/** Why a remote couldn't be reached. */
export const ReposUnreachableCause = z.enum(['dns', 'connection', 'host_key', 'auth']);
export type ReposUnreachableCause = z.infer<typeof ReposUnreachableCause>;

/** How to repair a fetch refspec naming a ref gone from origin. */
export const ReposRefGoneFix = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('unset_refspec'), pattern: z.string() }),
	z.strictObject({ kind: z.literal('set_branches'), branch: z.string().nullable() }),
	z.strictObject({ kind: z.literal('by_hand') })
]);
export type ReposRefGoneFix = z.infer<typeof ReposRefGoneFix>;

/**
 * Why a fetch or the visibility check failed, or why the tool refused to
 * fetch: Rust's `RemoteFailure` without `rejected`, which only a push meets.
 */
export const ReposFetchFailure = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('ref_gone'), refname: z.string(), fix: ReposRefGoneFix }),
	z.strictObject({
		kind: z.literal('unreachable'),
		cause: ReposUnreachableCause,
		message: z.string()
	}),
	z.strictObject({ kind: z.literal('repo_not_found'), message: z.string() }),
	z.strictObject({
		kind: z.literal('timed_out'),
		after_secs: Count.meta({ description: "the runner's timeout in seconds" })
	}),
	z.strictObject({ kind: z.literal('failed'), message: z.string() }),
	z.strictObject({ kind: z.literal('refspec_outside_origin'), refspec: z.string() }),
	z.strictObject({
		kind: z.literal('origin_refs_shared'),
		remote: z.string(),
		refspec: z.string()
	}),
	z.strictObject({ kind: z.literal('legacy_remotes_unreadable'), path: z.string() })
]);
export type ReposFetchFailure = z.infer<typeof ReposFetchFailure>;

/** What an anonymous read of a repo declared private found. */
export const ReposVisibilityCheck = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('leak') }),
	z.strictObject({ kind: z.literal('private') }),
	z.strictObject({ kind: z.literal('unknown'), failure: ReposFetchFailure })
]);
export type ReposVisibilityCheck = z.infer<typeof ReposVisibilityCheck>;

// --- entries ---

/** Why sync would stop on an entry and leave it to a person. */
export const ReposNeedsHuman = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('not_a_repo'), detail: z.string() }),
	z.strictObject({
		kind: z.literal('operation_in_progress'),
		checkout: z.string(),
		op: ReposInProgressOp
	}),
	z.strictObject({
		kind: z.literal('origin_mismatch'),
		origin: ReposOriginRemote,
		expected: z.string(),
		fix: ReposOriginFix
	}),
	z.strictObject({
		kind: z.literal('origin_not_https'),
		fetch_url: z.string(),
		expected: z.string(),
		fix: ReposOriginFix.nullable()
	}),
	z.strictObject({
		kind: z.literal('fetch_url_mismatch'),
		fetch_url: z.string(),
		expected: z.string(),
		fix: ReposOriginFix.nullable()
	}),
	z.strictObject({ kind: z.literal('worktree_unreadable'), path: z.string() }),
	z.strictObject({ kind: z.literal('default_branch_missing'), branch: z.string() }),
	z.strictObject({ kind: z.literal('default_branch_no_upstream'), branch: z.string() }),
	z.strictObject({ kind: z.literal('default_branch_gone'), branch: z.string() }),
	z.strictObject({ kind: z.literal('unexpected_detached'), checkout: z.string() }),
	z.strictObject({
		kind: z.literal('checkout_unresolvable'),
		checkout: z.string(),
		path: z.string(),
		error: z.string()
	}),
	z.strictObject({
		kind: z.literal('unlisted_git_dir'),
		git_dir: z.string(),
		head: ReposUnprobedHead,
		busy: z.array(ReposSession)
	}),
	z.strictObject({
		kind: z.literal('push_url_mismatch'),
		push_urls: z.array(z.string()),
		expected: z.string()
	}),
	z.strictObject({ kind: z.literal('clone_shares_repo'), with: z.string() }),
	z.strictObject({ kind: z.literal('cloned_unregistered'), dir: z.string() })
]);
export type ReposNeedsHuman = z.infer<typeof ReposNeedsHuman>;

/** One registry entry's state. */
export const ReposEntryStatus = z.strictObject({
	key: z.string(),
	kind: ReposEntryKind,
	dir: z.string(),
	url: z.string(),
	writable: z.boolean(),
	archived: z.boolean(),
	visibility: ReposVisibility.nullable(),
	ci: z.boolean(),
	branch: z.string().nullable(),
	pinned: z.boolean(),
	refresh: ReposRefreshVerdict.nullable(),
	presence: ReposPresence,
	clone: ReposCloneVerdict.nullable(),
	layout: ReposLayout.nullable(),
	checkouts: z.array(ReposCheckout),
	branches: z.array(ReposBranchStatus),
	at_rest: ReposAtRest.nullable(),
	stashes: Count,
	fetched_at: UnixSeconds.nullable(),
	needs_human: z.array(ReposNeedsHuman),
	probe_error: z.string().nullable(),
	unprobed_worktrees: z.array(ReposUnprobedWorktreeStatus),
	fetch_error: ReposFetchFailure.nullable(),
	visibility_check: ReposVisibilityCheck.nullable()
});
export type ReposEntryStatus = z.infer<typeof ReposEntryStatus>;

// --- unregistered clones ---

/** What keeps a moved worktree's repair from being offered. */
export const ReposRepairBlock = z.discriminatedUnion('kind', [
	z.strictObject({ kind: z.literal('rewrites'), path: z.string(), git_dir: z.string() }),
	z.strictObject({ kind: z.literal('claimed_dir'), git_dir: z.string() }),
	z.strictObject({ kind: z.literal('swapped'), git_dir: z.string(), with: z.string() }),
	z.strictObject({ kind: z.literal('relative_gitdir'), git_dir: z.string() }),
	z.strictObject({ kind: z.literal('unreadable_gitdir'), git_dir: z.string() }),
	z.strictObject({ kind: z.literal('non_utf8_path') }),
	z.strictObject({ kind: z.literal('nul_in_gitdir'), git_dir: z.string() })
]);
export type ReposRepairBlock = z.infer<typeof ReposRepairBlock>;

// `UnregisteredClone`'s own fields, beside its flattened `UnregisteredKind`
const unregistered_clone_fields = {
	dir: z.string(),
	origin: z.string().nullable(),
	owned: z.boolean()
};

/**
 * A child of the workspace root holding a `.git` that no registry entry
 * claims, its `kind` flattened beside its own fields.
 */
export const ReposUnregisteredClone = z.discriminatedUnion('kind', [
	z.strictObject({ ...unregistered_clone_fields, kind: z.literal('clone') }),
	z.strictObject({ ...unregistered_clone_fields, kind: z.literal('worktree') }),
	z.strictObject({
		...unregistered_clone_fields,
		kind: z.literal('moved_worktree'),
		entry: z.string(),
		blocked_by: ReposRepairBlock.nullable(),
		exit_noise: z.string().nullable()
	}),
	z.strictObject({
		...unregistered_clone_fields,
		kind: z.literal('orphaned_worktree'),
		entry: z.string()
	}),
	z.strictObject({
		...unregistered_clone_fields,
		kind: z.literal('shared_git_dir'),
		entry: z.string(),
		with: z.string().nullable()
	}),
	z.strictObject({ ...unregistered_clone_fields, kind: z.literal('unfinished_clone') })
]);
export type ReposUnregisteredClone = z.infer<typeof ReposUnregisteredClone>;

// --- the documents ---

/** The `repos status --json` report. */
export const ReposStatusReport = z.strictObject({
	version: z.literal(REPOS_STATUS_FORMAT_VERSION),
	workspace: z.string(),
	registry: z.string(),
	fetched: z.boolean(),
	sessions: ReposSessions,
	entries: z.array(ReposEntryStatus),
	unregistered: z
		.array(ReposUnregisteredClone)
		.nullable()
		.meta({ description: 'null when the scan did not run (a run with targets)' })
});
export type ReposStatusReport = z.infer<typeof ReposStatusReport>;

// `ErrorBody`'s own fields, beside its flattened `ErrorKind`
const error_body_fields = {
	message: z.string(),
	hint: z.string().nullable()
};

/**
 * A fatal error as `repos status --json` prints it, its `kind` flattened
 * beside `message` and `hint`. Only the kinds `status` can print: the rest
 * of Rust's `ErrorKind` are `repos push`'s, or precede knowing `--json`.
 */
export const ReposStatusErrorBody = z.discriminatedUnion('kind', [
	z.strictObject({ ...error_body_fields, kind: z.literal('references_with_targets') }),
	z.strictObject({ ...error_body_fields, kind: z.literal('root_not_found') }),
	z.strictObject({ ...error_body_fields, kind: z.literal('registry_not_found') }),
	z.strictObject({ ...error_body_fields, kind: z.literal('root_in_entry'), key: z.string() }),
	z.strictObject({ ...error_body_fields, kind: z.literal('registry_read') }),
	z.strictObject({ ...error_body_fields, kind: z.literal('registry_parse') }),
	z.strictObject({
		...error_body_fields,
		kind: z.literal('registry_invalid'),
		issues: z.array(ReposRegistryIssue)
	}),
	z.strictObject({ ...error_body_fields, kind: z.literal('git_not_found') }),
	z.strictObject({
		...error_body_fields,
		kind: z.literal('git_too_old'),
		found: z.string(),
		required: z.string()
	}),
	z.strictObject({
		...error_body_fields,
		kind: z.literal('unknown_entry'),
		name: z.string(),
		suggestions: z.array(z.string())
	}),
	z.strictObject({ ...error_body_fields, kind: z.literal('io') })
]);
export type ReposStatusErrorBody = z.infer<typeof ReposStatusErrorBody>;

/** The document `repos status --json` prints on a fatal error, in place of the report. */
export const ReposStatusErrorReport = z.strictObject({
	version: z.literal(REPOS_STATUS_FORMAT_VERSION),
	error: ReposStatusErrorBody
});
export type ReposStatusErrorReport = z.infer<typeof ReposStatusErrorReport>;

/**
 * Either document `repos status --json` prints. No tag is needed: `error`
 * is the error document's alone, and each side's strictness refuses the
 * other's fields. A consumer wanting a sharper parse error branches on
 * `error` first and parses with that side's schema.
 */
export const ReposStatusDocument = z.union([ReposStatusReport, ReposStatusErrorReport]);
export type ReposStatusDocument = z.infer<typeof ReposStatusDocument>;
