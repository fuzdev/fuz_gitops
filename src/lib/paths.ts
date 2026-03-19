/**
 * Base directory for all gitops-generated files.
 */
export const GITOPS_OUTPUT_DIR = '.gro/fuz_gitops';

/**
 * Default repos directory relative to gitops config file.
 * Resolves to the parent of the directory with the config
 * (e.g., `~/dev/repo/gitops.config.ts` resolves to `~/dev/`).
 */
export const DEFAULT_REPOS_DIR = '..';
