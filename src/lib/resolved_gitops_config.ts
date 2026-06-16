import type {GitopsConfig} from './gitops_config.ts';
import {local_repo_locate, type LocalRepoPath, type LocalRepoMissing} from './local_repo.ts';

export interface ResolvedGitopsConfig {
	local_repos: Array<LocalRepoPath | LocalRepoMissing> | null;
	local_repo_paths: Array<LocalRepoPath> | null;
	local_repos_missing: Array<LocalRepoMissing> | null;
}

export const resolve_gitops_config = (
	gitops_config: GitopsConfig,
	repos_dir: string,
): ResolvedGitopsConfig => {
	const local_repos = gitops_config.repos.map((r) =>
		local_repo_locate({repo_config: r, repos_dir}),
	);

	const local_repo_paths = local_repos.filter((r) => r.type === 'local_repo_path');
	const local_repos_missing = local_repos.filter((r) => r.type === 'local_repo_missing');

	const config: ResolvedGitopsConfig = {
		local_repos: local_repos.length ? local_repos : null,
		local_repo_paths: local_repo_paths.length ? local_repo_paths : null,
		local_repos_missing: local_repos_missing.length ? local_repos_missing : null,
	};
	return config;
};
