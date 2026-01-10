<script lang="ts">
	import Svg from '@fuzdev/fuz_ui/Svg.svelte';
	import {logo_fuz} from '@fuzdev/fuz_ui/logos.js';
</script>

<header class="pane p_lg">
	<div class="box bg mb_lg">
		<h1 class="mb_lg font_size_xl2 font-weight:400">fuz_gitops</h1>
		<!-- TODO FuzGitopsLogo -->
		<Svg
			data={logo_fuz}
			fill="var(--color_a_5)"
			size="var(--icon_size_xl2)"
			label="a friendly blue spider facing you"
		/>
	</div>
	<p class="text-align:center">a tool for managing many repos 🪄</p>
</header>
