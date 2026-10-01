<script lang="ts">
	import { resolve } from '$app/paths';

	import { to_pull_requests, to_pull_url, type FilterPullRequest } from './github_helpers.ts';
	import type { Repo } from './repo.svelte.ts';

	const {
		repos,
		filter_pull_request
	}: {
		repos: Array<Repo>;
		filter_pull_request?: FilterPullRequest | undefined;
	} = $props();

	const pull_requests = $derived(to_pull_requests(repos, filter_pull_request));
</script>

<div class="width_atmost_md">
	<section class="panel p_sm">
		<table>
			<thead>
				<tr>
					<th>repo</th>
					<th>number</th>
					<th>title</th>
				</tr>
			</thead>
			<tbody>
				<!-- PR numbers repeat across repos, so the key pairs them with the repo -->
				{#each pull_requests as { repo, pull_request } (`${repo.name}#${pull_request.number}`)}
					<tr>
						<td>
							<a href={resolve(`/tree/${repo.repo_name}`)}>
								{repo.repo_name}{#if repo.package_json.glyph}
									&nbsp;{repo.package_json.glyph}
								{/if}
							</a>
						</td>
						<td>
							<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
								href={to_pull_url(repo.repo_url, pull_request)}
								title={pull_request.title}
							>
								#{pull_request.number}
							</a>
						</td>
						<td><div>{pull_request.title}</div></td>
					</tr>
				{/each}
			</tbody>
		</table>
	</section>
</div>

<style>
	th,
	td {
		padding: 0 var(--space_md);
	}
	section {
		margin-bottom: var(--space_xl5);
	}
</style>
