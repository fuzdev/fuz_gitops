<script lang="ts">
	import LibrarySummary from '@fuzdev/fuz_ui/LibrarySummary.svelte';
	import LibraryDetail from '@fuzdev/fuz_ui/LibraryDetail.svelte';
	import {resolve} from '$app/paths';
	import {format_url} from '@fuzdev/fuz_util/url.js';
	import type {Snippet} from 'svelte';

	import type {Repo} from './repo.svelte.js';
	import ReposTreeNav from './ReposTreeNav.svelte';

	const {
		repos,
		selected_repo,
		nav,
	}: {
		repos: Array<Repo>;
		selected_repo?: Repo | undefined;
		nav: Snippet;
	} = $props();
</script>

<div class="repos_tree">
	<ReposTreeNav {repos} {selected_repo}>
		{@render nav()}
	</ReposTreeNav>
	{#if selected_repo}
		<section class="detail_wrapper">
			<div class="panel detail p_md">
				<LibraryDetail library={selected_repo.library} />
			</div>
		</section>
	{:else}
		<menu class="summaries">
			{#each repos as repo (repo.name)}
				<li class="panel p_md box">
					{#if repo.package_json}
						<LibrarySummary library={repo.library}>
							{#snippet repo_name(repo_name)}
								<a href={resolve(`/tree/${repo_name}`)} class="repo_name">{repo_name}</a>
							{/snippet}
						</LibrarySummary>
					{:else}
						<div class="width_atmost_sm">
							<p>
								failed to fetch <code>.well-known/package.json</code> from
								<!-- eslint-disable-next-line svelte/no-navigation-without-resolve --><a
									href={repo.repo_url}>{format_url(repo.repo_url)}</a
								>
							</p>
						</div>
					{/if}
				</li>
			{/each}
		</menu>
	{/if}
</div>

<style>
	.repos_tree {
		width: 100%;
		display: flex;
		flex-direction: row;
		align-items: flex-start;
	}
	.summaries {
		padding: var(--space_lg);
		gap: var(--space_lg);
		display: flex;
		flex-direction: row;
		flex-wrap: wrap;
		justify-content: flex-start;
		align-items: flex-start;
	}
	.summaries li {
		margin-bottom: var(--space_xl);
	}
	.repo_name {
		font-size: var(--font_size_xl2);
		font-weight: 500;
		text-align: center;
		margin-bottom: var(--space_xl);
	}
	.detail_wrapper {
		padding: var(--space_lg);
		width: 100%;
	}
	.detail {
		display: flex;
	}
</style>
