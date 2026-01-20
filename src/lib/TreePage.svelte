<script lang="ts">
	import Breadcrumb from '@fuzdev/fuz_ui/Breadcrumb.svelte';

	import PageFooter from './PageFooter.svelte';
	import PageHeader from './PageHeader.svelte';
	import ReposTree from './ReposTree.svelte';
	import type {Repo} from './repo.svelte.js';

	const {
		repo,
		repos,
	}: {
		repo: Repo;
		repos: Array<Repo>;
	} = $props();

	// TODO ideally there would be one `ReposTree` mounted by the layout with transitions
</script>

<svelte:head>
	<title>tree {repo.package_json.glyph} {repo.name}</title>
</svelte:head>

<main class="box width:100%">
	<div class="p_lg">
		<PageHeader {repo} />
	</div>
	<section class="tree">
		<ReposTree {repos}>
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
		justify-content: center;
	}
	section:first-child {
		margin-top: var(--space_xl4);
	}
	.tree {
		display: flex;
		flex-direction: row;
		align-items: flex-start;
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
