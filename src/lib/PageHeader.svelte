<script lang="ts">
	import type {PackageJson} from '@fuzdev/fuz_util/package_json.js';
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
		repo: {package_json: PackageJson} | {url: string; package_json: null};
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
		<nav {...nav_attrs}><Breadcrumb>{repo.package_json?.glyph}</Breadcrumb></nav>
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
