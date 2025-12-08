<script lang="ts">
	import PageFooter from './PageFooter.svelte';
	import PageHeader from './PageHeader.svelte';
	import ReposTable from './ReposTable.svelte';
	import type {Repo} from './repo.svelte.js';

	const {
		repo,
		repos,
	}: {
		repo: Repo;
		repos: Array<Repo>;
	} = $props();
</script>

<svelte:head>
	<title>table {repo.package_json.glyph} {repo.package_json.name}</title>
</svelte:head>

<main class="box">
	<div class="p_lg">
		<PageHeader {repo} />
	</div>
	<section>
		<div class="panel p_md">
			<ReposTable {repos} />
		</div>
	</section>
	<section class="box mb_xl7">
		<PageFooter />
	</section>
</main>

<style>
	main {
		display: flex;
		flex-direction: column;
		align-items: center;
		margin: 0 auto;
	}
	section {
		margin-bottom: var(--space_xl4);
	}
	section:first-child {
		margin-top: var(--space_xl4);
	}
</style>
