<script lang="ts">
	import {resolve} from '$app/paths';
	import type {Snippet} from 'svelte';

	import type {Repo} from './repo.svelte.js';

	const {
		repos,
		selected_repo,
		children,
	}: {
		repos: Array<Repo>;
		selected_repo?: Repo;
		children: Snippet;
	} = $props();
</script>

<nav>
	<menu class="panel p_md">
		{#each repos as repo (repo.name)}
			{@const selected = repo === selected_repo}
			<li style:display="contents">
				{#if repo.package_json}<a
						class="menu_item"
						class:selected
						href={resolve(`/tree/${repo.repo_name}`)}
						><div class="ellipsis">
							{repo.repo_name}{#if repo.package_json.glyph}&nbsp;{repo.package_json.glyph}{/if}
						</div></a
					>{/if}
			</li>
		{/each}
	</menu>
	{@render children()}
</nav>

<style>
	nav {
		display: flex;
		flex-direction: column;
		position: sticky;
		top: var(--space_md);
		width: var(--nav_width, 240px);
		padding: var(--space_lg);
		padding-right: 0;
	}
</style>
