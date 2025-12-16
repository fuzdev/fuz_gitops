/**
 * Shared constants for gitops tasks and operations.
 *
 * Naming convention: GITOPS_{NAME}_DEFAULT for user-facing defaults.
 */

/**
 * Maximum number of iterations for fixed-point iteration during publishing.
 * Used in both plan generation and actual publishing to resolve transitive dependency cascades.
 *
 * In practice, most repos converge in 2-3 iterations.
 * Deep dependency chains may require more iterations.
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
