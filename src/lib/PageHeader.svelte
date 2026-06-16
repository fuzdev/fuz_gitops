<script lang="ts">
	import type {PkgJson} from '@fuzdev/fuz_util/pkg_json.ts';
	import Breadcrumb from '@fuzdev/fuz_ui/Breadcrumb.svelte';
	import type {Snippet} from 'svelte';
	import type {SvelteHTMLElements} from 'svelte/elements';

	const {
		repo,
		nav_attrs,
		attrs,
		nav,
		children,
	}: {
		repo: {pkg_json: PkgJson} | {url: string; pkg_json: null};
		nav_attrs?: SvelteHTMLElements['nav'];
		attrs?: SvelteHTMLElements['header'];
		nav?: Snippet;
		children?: Snippet;
	} = $props();
</script>

<header {...attrs}>
	{@render children?.()}
	{#if nav}
		{@render nav()}
	{:else}
		<nav {...nav_attrs}><Breadcrumb>{repo.pkg_json?.glyph}</Breadcrumb></nav>
	{/if}
</header>

<style>
	header {
		--font_size: var(--font_size_xl);
	}
	nav {
		display: flex;
		justify-content: center;
	}
</style>
