<script lang="ts">
	import '$routes/fuz.css';
	import '@fuzdev/fuz_code/theme.css';

	import Themed from '@fuzdev/fuz_ui/Themed.svelte';
	import Dialog from '@fuzdev/fuz_ui/Dialog.svelte';
	import ContextmenuRoot from '@fuzdev/fuz_ui/ContextmenuRoot.svelte';
	import {
		ContextmenuState,
		contextmenu_attachment,
	} from '@fuzdev/fuz_ui/contextmenu_state.svelte.js';
	import {Library, library_context} from '@fuzdev/fuz_ui/library.svelte.js';
	import type {Snippet} from 'svelte';

	import Settings from '$routes/Settings.svelte';
	import {repos_json} from '$routes/repos.js';
	import {Repo, type RepoJson, repos_parse, repos_context} from '$lib/repo.svelte.js';
	import {library_json} from '$routes/library.js';

	const {
		children,
	}: {
		children: Snippet;
	} = $props();

	const contextmenu = new ContextmenuState();

	const repos = repos_parse(
		repos_json.map((r: RepoJson) => new Repo(r)),
		'https://gitops.fuz.dev/',
	);
	repos_context.set(repos);
	library_context.set(new Library(library_json));

	let show_settings = $state(false);
</script>

<svelte:head>
	<title>@fuzdev/fuz_gitops</title>
</svelte:head>

<svelte:body
	{@attach contextmenu_attachment([
		{
			snippet: 'text',
			props: {
				content: 'Settings',
				icon: '?',
				run: () => {
					show_settings = true;
				},
			},
		},
		{
			snippet: 'text',
			props: {
				content: 'Reload',
				icon: '⟳',
				run: () => {
					location.reload();
				},
			},
		},
	])}
/>

<Themed>
	<ContextmenuRoot {contextmenu}>
		{@render children()}
	</ContextmenuRoot>
	{#if show_settings}
		<Dialog onclose={() => (show_settings = false)}>
			<div class="pane p_md width_atmost_md mx_auto">
				<Settings />
			</div>
		</Dialog>
	{/if}
</Themed>
