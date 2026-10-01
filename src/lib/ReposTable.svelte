<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { format_url } from '@fuzdev/fuz_util/url.ts';

	import type { Repo } from './repo.svelte.ts';
	import { to_pull_url } from './github_helpers.ts';

	const {
		repos,
		deps = ['@fuzdev/fuz_ui', '@fuzdev/gro']
	}: {
		repos: Array<Repo>;
		deps?: Array<string>;
	} = $props();

	// TODO fade out the `version` column if all deps are upgraded to the latest

	// TODO gray out the latest of each version for deps, but only if the max is knowable via a local dep, don't assume for externals

	// TODO hacky, handle regular deps too
	const lookup_dep_version = (repo: Repo, dep: string): string | undefined => {
		for (const key in repo.package_json.dependencies) {
			if (key === dep) {
				return repo.package_json.dependencies[key];
			}
		}
		for (const key in repo.package_json.devDependencies) {
			if (key === dep) {
				return repo.package_json.devDependencies[key];
			}
		}
		return undefined;
	};

	const latest_version_by_dep = $derived(
		new Map<string, string | null>(
			deps.map((dep) => {
				const repo = repos.find((repo) => repo.package_json.name === dep);
				return [dep, repo?.package_json.version ?? null];
			})
		)
	);

	const format_version = (version: string | null | undefined): string =>
		version == null ? '' : version.replace(/^(\^|>=)\s*/, '');
</script>

<table>
	<thead>
		<tr>
			<th>tree</th>
			<th>homepage</th>
			<th>repo</th>
			<th>npm</th>
			<th>version</th>
			{#each deps as dep (dep)}
				<th>{dep}</th>
			{/each}
			<th>pull requests</th>
		</tr>
	</thead>
	<tbody>
		{#each repos as repo (repo.name)}
			{@const { package_json, homepage_url } = repo}
			{@const check_runs = repo.check_runs}
			{@const check_runs_completed = check_runs?.status === 'completed'}
			{@const check_runs_success = check_runs?.conclusion === 'success'}
			<tr>
				<td>
					<div class="row">
						<a href={resolve(`/tree/${repo.repo_name}`)}>{package_json.glyph ?? '🌳'}</a>
					</div>
				</td>
				<td>
					<div class="row">
						{#if homepage_url}
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
							<a class:selected={homepage_url === page.url.href} href={homepage_url} class="row">
								<img
									src={repo.logo_url}
									alt={repo.logo_alt}
									style:width="16px"
									style:height="16px"
									style:margin-right="var(--space_xs)"
								/>
								{format_url(homepage_url)}
							</a>
						{/if}
					</div>
				</td>
				<td>
					<div class="row">
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
						<a href={repo.repo_url}>{repo.repo_name}</a>
						{#if check_runs && (!check_runs_completed || !check_runs_success)}
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
								href="{repo.repo_url}/commits/{repo.branch}"
								title={!check_runs_completed
									? `status: ${check_runs.status}`
									: `CI failed: ${check_runs.conclusion}`}
							>
								{#if !check_runs_completed}🟡{:else}⚠️{/if}
							</a>
						{/if}
					</div>
				</td>
				<td>
					{#if repo.npm_url}
						<div class="row">
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
								href={repo.npm_url}
							>
								<code>{repo.name}</code>
							</a>
						</div>
					{/if}
				</td>
				<td>
					{#if package_json.version !== '0.0.1'}
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
							href={repo.changelog_url}
						>
							{format_version(package_json.version)}
						</a>
					{/if}
				</td>
				{#each deps as dep (dep)}
					{@const dep_version = lookup_dep_version(repo, dep)}
					{@const formatted_dep_version = format_version(dep_version)}
					{@const dep_latest_version = latest_version_by_dep.get(dep)}
					<td>
						<div
							class:latest={!!dep_latest_version && formatted_dep_version === dep_latest_version}
						>
							{formatted_dep_version}
						</div>
					</td>
				{/each}
				<td>
					<!-- TODO show something like `and N more` with a link to a dialog list -->
					<div class="row">
						{#if repo.pull_requests}
							{#each repo.pull_requests as pull (pull.number)}
								<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
									href={to_pull_url(repo.repo_url, pull)}
									class="chip"
									title={pull.title}
								>
									#{pull.number}
								</a>
							{/each}
						{/if}
					</div>
				</td>
			</tr>
		{/each}
	</tbody>
</table>

<style>
	/* TODO add basic table styles upstream and delete this */
	th {
		text-align: left;
	}
	th,
	td {
		padding: 0 var(--space_xs);
	}
	.latest {
		color: var(--text_50);
	}
</style>
