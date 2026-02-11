<script lang="ts">
	import {page} from '$app/state';
	import {resolve} from '$app/paths';
	import {format_url} from '@fuzdev/fuz_util/url.js';

	import type {Repo} from './repo.svelte.js';
	import {to_pull_url} from './github_helpers.js';

	const {
		repos,
		deps = ['@fuzdev/fuz_ui', '@fuzdev/gro'],
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
				if (!repo?.package_json) return [dep, null];
				return [dep, repo.package_json.version];
			}),
		),
	);

	const format_version = (version: string | null | undefined): string =>
		version == null ? '' : version.replace(/^(\^|>=)\s*/, '');

	const lookup_pull_requests = (repos: Array<Repo> | null, repo: Repo) => {
		const found = repos?.find((p) => p.repo_url === repo.repo_url);
		if (!found?.package_json) return null;
		const {pull_requests} = found;
		return pull_requests;
	};
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
			{@const {package_json, homepage_url} = repo}
			<tr>
				<td>
					<div class="row">
						{#if package_json}
							<a href={resolve(`/tree/${repo.repo_name}`)}>{package_json.glyph ?? '🌳'}</a>
						{/if}
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
						{#if package_json}
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
							<a href={repo.repo_url}>{repo.repo_name}</a>
							{@const check_runs = repo.check_runs}
							{@const check_runs_completed = check_runs?.status === 'completed'}
							{@const check_runs_success = check_runs?.conclusion === 'success'}
							{#if check_runs && (!check_runs_completed || !check_runs_success)}
								<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
									href="{repo.repo_url}/commits/main"
									title={!check_runs_completed
										? `status: ${check_runs.status}`
										: `CI failed: ${check_runs.conclusion}`}
									>{#if !check_runs_completed}🟡{:else}⚠️{/if}</a
								>
							{/if}
						{:else}
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
								href={repo.repo_url}>{format_url(repo.repo_url)}</a
							>
						{/if}
					</div>
				</td>
				<td>
					{#if repo.npm_url}
						<div class="row">
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
								href={repo.npm_url}><code>{repo.name}</code></a
							>
						</div>
					{/if}
				</td>
				<td>
					{#if package_json.version !== '0.0.1'}
						<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
							href={repo.changelog_url}>{format_version(package_json.version)}</a
						>
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
					{#if repo.repo_url}
						{@const pull_requests = lookup_pull_requests(repos, repo)}
						<!-- TODO show something like `and N more` with a link to a dialog list -->
						<div class="row">
							{#if pull_requests}
								{#each pull_requests as pull (pull)}
									<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
										href={to_pull_url(repo.repo_url, pull)}
										class="chip"
										title={pull.title}>#{pull.number}</a
									>
								{/each}
							{/if}
						</div>
					{/if}
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
	tr:hover {
		background-color: var(--fg_50);
	}
	.latest {
		/* TODO even lighter, add `--text_color_7` to Fuz probably, or change the scaling of `--text_color_3` and `--text_color_5` */
		color: var(--text_50);
		opacity: var(--disabled_opacity);
	}
</style>
