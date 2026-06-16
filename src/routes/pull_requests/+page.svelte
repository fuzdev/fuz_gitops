<script lang="ts">
	import PullRequestsPage from '$lib/PullRequestsPage.svelte';
	import {repos_context} from '$lib/repo.svelte.ts';

	const {repo, repos} = repos_context.get();
</script>

<PullRequestsPage {repo} {repos} filter_pull_request={(pull) => pull.user.login === 'ryanatkn'} />
