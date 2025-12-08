<script lang="ts">
	import PageFooter from './PageFooter.svelte';
	import PageHeader from './PageHeader.svelte';
	import PullRequestsDetail from './PullRequestsDetail.svelte';
	import type {FilterPullRequest} from './github_helpers.js';
	import type {Repo} from './repo.svelte.js';

	const {
		repo,
		repos,
		filter_pull_request,
	}: {
		repo: Repo;
		repos: Array<Repo>;
		filter_pull_request?: FilterPullRequest | undefined;
	} = $props();
</script>

<svelte:head>
	<title>pull requests {repo.package_json.glyph} {repo.package_json.name}</title>
</svelte:head>

<main class="box width_100">
	<div class="p_lg">
		<PageHeader {repo} />
	</div>
	<section>
		<PullRequestsDetail {repos} {filter_pull_request} />
	</section>
	<section class="box mb_xl7">
		<PageFooter />
	</section>
</main>

<style>
	section {
		width: 100%;
		margin-bottom: var(--space_xl4);
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
	}
	section:first-child {
		margin-top: var(--space_xl4);
	}
</style>
