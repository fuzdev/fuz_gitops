<script lang="ts">
	import 'virtual:fuz.css';
	import '@fuzdev/fuz_code/theme.css';

	import ThemeRoot from '@fuzdev/fuz_ui/ThemeRoot.svelte';
	import Dialog from '@fuzdev/fuz_ui/Dialog.svelte';
	import DialogContent from '@fuzdev/fuz_ui/DialogContent.svelte';
	import ContextmenuRoot from '@fuzdev/fuz_ui/ContextmenuRoot.svelte';
	import {
		ContextmenuState,
		contextmenu_attachment
	} from '@fuzdev/fuz_ui/contextmenu_state.svelte.ts';
	import { SiteState, site_context } from '@fuzdev/fuz_ui/site.svelte.ts';
	import { logo_fuz_gitops } from '@fuzdev/fuz_ui/logos.ts';
	import type { Snippet } from 'svelte';
	import pkg_json from 'virtual:pkg.json';

	import Settings from './Settings.svelte';
	import { repos_json } from './repos.ts';
	import { Repo, type RepoJson, repos_parse, repos_context } from '$lib/repo.svelte.ts';

	const {
		children
	}: {
		children: Snippet;
	} = $props();

	const contextmenu = new ContextmenuState();

	const repos = repos_parse(
		repos_json.map((r: RepoJson) => new Repo(r)),
		'https://gitops.fuz.dev/'
	);
	repos_context.set(repos);
	// `glyph` and `repo_url` derive from `pkg_json`; `icon` stays explicit (structured `SvgData`).
	site_context.set(new SiteState({ icon: logo_fuz_gitops, pkg_json }));

	let show_settings = $state.raw(false);
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
				}
			}
		},
		{
			snippet: 'text',
			props: {
				content: 'Reload',
				icon: '⟳',
				run: () => {
					location.reload();
				}
			}
		}
	])}
/>

<ThemeRoot>
	<ContextmenuRoot {contextmenu}>
		{@render children()}
	</ContextmenuRoot>
	{#if show_settings}
		<Dialog onclose={() => (show_settings = false)}>
			<DialogContent>
				<Settings />
			</DialogContent>
		</Dialog>
	{/if}
</ThemeRoot>
