<script lang="ts">
	import LibraryDetail from '@fuzdev/fuz_ui/LibraryDetail.svelte';
	import EcosystemLinksPanel from '@fuzdev/fuz_ui/EcosystemLinksPanel.svelte';
	import {library_context} from '@fuzdev/fuz_ui/library.svelte.js';

	import PageFooter from '$lib/PageFooter.svelte';
	import PageHeader from '$lib/PageHeader.svelte';

	const library = library_context.get();
</script>

<svelte:head>
	<title>about {library.package_json.glyph} {library.package_json.name}</title>
</svelte:head>

<main class="width_upto_md">
	<div class="p_lg">
		<PageHeader repo={library}></PageHeader>
	</div>
	<EcosystemLinksPanel />
	<section class="box width:100% mb_lg">
		<div class="panel p_md width_upto_md">
			<LibraryDetail {library} />
		</div>
	</section>
	<PageFooter />
</main>

<style>
	main {
		display: flex;
		flex-direction: column;
		align-items: center;
		margin: 0 auto;
	}
</style>
