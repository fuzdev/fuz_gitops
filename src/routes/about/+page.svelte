<script lang="ts">
	import LibraryDetail from '@fuzdev/fuz_ui/LibraryDetail.svelte';
	import EcosystemLinksPanel from '@fuzdev/fuz_ui/EcosystemLinksPanel.svelte';
	import {Library, library_context} from '@fuzdev/fuz_ui/library.svelte.js';
	import {site_context} from '@fuzdev/fuz_ui/site.svelte.js';
	import {library_json} from '$routes/library.js';

	import PageFooter from '$lib/PageFooter.svelte';
	import PageHeader from '$lib/PageHeader.svelte';

	const library = library_context.set(new Library(library_json));
	const site = site_context.get();
</script>

<svelte:head>
	<title>about {site.glyph} fuz_gitops</title>
</svelte:head>

<main class="width_atmost_md box mx_auto">
	<div class="p_lg">
		<PageHeader repo={library}></PageHeader>
	</div>
	<EcosystemLinksPanel />
	<section class="box width:100% mb_lg">
		<div class="panel p_md width_atmost_md">
			<LibraryDetail {library} />
		</div>
	</section>
	<PageFooter />
</main>
