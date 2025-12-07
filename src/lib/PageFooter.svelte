<script lang="ts">
	import DocsFooter from '@fuzdev/fuz_ui/DocsFooter.svelte';
	import Breadcrumb from '@fuzdev/fuz_ui/Breadcrumb.svelte';
	import {library_context} from '@fuzdev/fuz_ui/library.svelte.js';

	const library = library_context.get();
</script>

<section class="box mb_xl7">
	<DocsFooter {library}>
		<nav class="mb_xl5">
			<Breadcrumb>{library.package_json.glyph}</Breadcrumb>
		</nav>
	</DocsFooter>
</section>
