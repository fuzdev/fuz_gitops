<script lang="ts">
	import LibraryDetail from '@fuzdev/fuz_ui/LibraryDetail.svelte';
	import {Library, library_context} from '@fuzdev/fuz_ui/library.svelte.js';

	const {
		library,
	}: {
		library: Library;
	} = $props();

	// `LibraryDetail`'s `ModuleLink`/`DeclarationLink` resolve against `library_context`,
	// so set it to this repo's library. The caller keys this component on the repo,
	// so it remounts (and re-sets context) when the selected repo changes.
	// svelte-ignore state_referenced_locally
	library_context.set(library);
</script>

<!--
	`links_full` points the module/declaration links at each repo's own deployed
	docs (`homepage_url`-based) rather than this site's local `/docs/api/*`, which
	only knows fuz_gitops's own modules — otherwise the foreign links dangle.
-->
<LibraryDetail {library} links_full />
