/**
 * Shared constants for gitops tasks and operations.
 *
 * Naming convention: GITOPS_{NAME}_DEFAULT for user-facing defaults.
 *
 * @module
 */

/**
 * Maximum number of fixed-point iterations plan generation runs to resolve
 * transitive dependency cascades. Publishing executes the frozen plan in a
 * single pass and doesn't iterate.
 *
 * Each iteration reaches at least one more level of dependents, so a deep dependency
 * chain needs more; a plan that hits the limit still changing warns, naming
 * the packages left.
 */
export const GITOPS_MAX_ITERATIONS_DEFAULT = 10;

/**
 * Default path to the gitops configuration file.
 */
export const GITOPS_CONFIG_PATH_DEFAULT = 'gitops.config.ts';

/**
 * Default number of repos to process concurrently during parallel operations.
 */
export const GITOPS_CONCURRENCY_DEFAULT = 5;

/**
 * Default timeout in milliseconds for waiting on NPM package propagation (10 minutes).
 * NPM's CDN uses eventual consistency, so published packages may not be immediately available.
 */
export const GITOPS_NPM_WAIT_TIMEOUT_DEFAULT = 600_000; // 10 minutes
