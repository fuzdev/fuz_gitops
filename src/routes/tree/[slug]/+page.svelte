<script lang="ts">
	import TreeItemPage from '$lib/TreeItemPage.svelte';
	import {repos_context} from '$lib/repo.svelte.ts';

	const {params} = $props();

	const {repo, repos} = repos_context.get();
</script>

<TreeItemPage {repo} {repos} slug={params.slug} />
