<script lang="ts">
	import Alert from '@fuzdev/fuz_ui/Alert.svelte';
	import Breadcrumb from '@fuzdev/fuz_ui/Breadcrumb.svelte';

	import PageFooter from './PageFooter.svelte';
	import PageHeader from './PageHeader.svelte';
	import ReposTree from './ReposTree.svelte';
	import type {Repo} from './repo.svelte.js';

	const {
		repo,
		repos,
		slug,
	}: {
		repo: Repo;
		repos: Array<Repo>;
		slug: string;
	} = $props();

	// TODO ideally there would be one `ReposTree` mounted by the layout with transitions

	const route_repo = $derived(repos.find((p) => p.repo_name === slug));
</script>

<svelte:head>
	<title>{slug} - tree {repo.package_json.glyph} {repo.package_json.name}</title>
</svelte:head>

<main class="box width:100%">
	<div class="p_lg">
		<PageHeader {repo} />
	</div>
	<section class="tree">
		{#if !route_repo}
			<div class="mb_lg">
				<Alert status="error"><p>cannot find <code>{slug}</code></p></Alert>
			</div>
		{/if}
		<ReposTree {repos} selected_repo={route_repo}>
			{#snippet nav()}
				<div class="repos_tree_nav">
					<Breadcrumb>{repo.package_json.glyph}</Breadcrumb>
				</div>
			{/snippet}
		</ReposTree>
	</section>
	<section class="box mb_xl7">
		<PageFooter />
	</section>
</main>

<style>
	section {
		width: 100%;
		margin-bottom: var(--space_xl4);
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
	}
	section:first-child {
		margin-top: var(--space_xl4);
	}
	.tree {
		display: flex;
		flex-direction: column;
		align-items: center;
		width: 100%;
	}
	.repos_tree_nav {
		display: flex;
		margin-top: var(--space_xl);
	}
	/* TODO hacky */
	.repos_tree_nav :global(.breadcrumb) {
		justify-content: flex-start;
	}
</style>
