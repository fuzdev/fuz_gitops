<script lang="ts">
	import TreePage from '$lib/TreePage.svelte';
	import { repos_context } from '$lib/repo.svelte.ts';

	const { repo, repos } = repos_context.get();

	// TODO ideally there would be one `ReposTree` mounted by the layout
</script>

<TreePage {repo} {repos} />
