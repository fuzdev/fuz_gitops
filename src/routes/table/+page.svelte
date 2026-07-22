<script lang="ts">
	import TablePage from '$lib/TablePage.svelte';
	import { repos_context } from '$lib/repo.svelte.ts';

	const { repo, repos } = repos_context.get();
</script>

<TablePage {repo} {repos} />
