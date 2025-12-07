<script lang="ts">
	import Breadcrumb from '@fuzdev/fuz_ui/Breadcrumb.svelte';

	import PageFooter from './PageFooter.svelte';
	import PageHeader from './PageHeader.svelte';
	import ModulesDetail from './ModulesDetail.svelte';
	import type {Repo} from './repo.svelte.js';

	interface Props {
		repo: Repo;
		repos: Array<Repo>;
	}

	const {repo, repos}: Props = $props();
</script>

<svelte:head>
	<title>modules {repo.package_json.glyph} {repo.package_json.name}</title>
</svelte:head>

<main class="box width_100">
	<div class="p_lg">
		<PageHeader {repo} />
	</div>
	<section>
		<ModulesDetail {repos}>
			{#snippet nav_footer()}
				<nav class="row">
					<Breadcrumb>{repo.package_json.glyph}</Breadcrumb>
				</nav>
			{/snippet}
		</ModulesDetail>
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
</style>
