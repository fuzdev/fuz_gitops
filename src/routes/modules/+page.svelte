<script lang="ts">
	import ModulesPage from '$lib/ModulesPage.svelte';
	import {repos_context} from '$lib/repo.svelte.ts';

	const {repo, repos} = repos_context.get();
</script>

<ModulesPage {repo} {repos} />
