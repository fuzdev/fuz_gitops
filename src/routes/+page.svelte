<script lang="ts">
	import DocsFooter from '@fuzdev/fuz_ui/DocsFooter.svelte';
	import Card from '@fuzdev/fuz_ui/Card.svelte';
	import {resolve} from '$app/paths';
	import {library_context} from '@fuzdev/fuz_ui/library.svelte.js';

	import MainHeader from '$routes/MainHeader.svelte';

	const library = library_context.get();
</script>

<svelte:head>
	<title>{library.package_json.name}</title>
</svelte:head>

<main class="box">
	<section style:margin-top="var(--docs_primary_nav_height, 60px)">
		<MainHeader />
	</section>
	<section>
		<menu class="unstyled">
			<li>
				<Card href={resolve('/docs')}
					>docs{#snippet icon()}{library.package_json.glyph}{/snippet}</Card
				>
			</li>
			<li>
				<Card href={resolve('/tree')} icon="">tree</Card>
			</li>
			<li>
				<Card href={resolve('/table')} icon="">table</Card>
			</li>
			<li>
				<Card href={resolve('/modules')} icon="">modules</Card>
			</li>
			<li>
				<Card href={resolve('/pull_requests')} icon="">pull requests</Card>
			</li>
		</menu>
	</section>
	<section class="mb_xl7">
		<DocsFooter {library}>
			{#snippet logo_header()}<a href={resolve('/about')} class="mb_xs">about</a>{/snippet}
			{#snippet logo_footer()}<a href="https://www.fuz.dev/" class="mt_xs">fuz.dev</a>{/snippet}
		</DocsFooter>
	</section>
</main>

<style>
	main {
		display: flex;
		flex-direction: column;
		align-items: center;
		margin: 0 auto;
	}
	menu {
		gap: var(--space_lg);
		width: 100%;
		display: flex;
		flex-wrap: wrap;
		flex-direction: column;
		align-items: center;
		font-size: var(--font_size_xl3);
	}
	li {
		margin-bottom: var(--space_xl);
	}
</style>
