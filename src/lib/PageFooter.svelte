<script lang="ts">
	import DocsFooter from '@fuzdev/fuz_ui/DocsFooter.svelte';
	import { site_context } from '@fuzdev/fuz_ui/site.svelte.ts';
	import { FUZ_DEV_URL } from '@fuzdev/fuz_ui/constants.ts';
	import Breadcrumb from '@fuzdev/fuz_ui/Breadcrumb.svelte';

	const site = site_context.get();
</script>

<section class="box mb_xl7">
	<DocsFooter repo_url={site.repo_url} root_url={FUZ_DEV_URL}>
		<nav class="mb_xl5">
			<Breadcrumb />
		</nav>
	</DocsFooter>
</section>
