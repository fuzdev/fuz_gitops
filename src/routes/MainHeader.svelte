<script lang="ts">
	import Svg from '@fuzdev/fuz_ui/Svg.svelte';
	import {fuz_logo} from '@fuzdev/fuz_ui/logos.js';
</script>

<header class="pane p_lg">
	<div class="box bg mb_lg">
		<h1 class="mb_lg font_size_xl2 font_weight_400">fuz_gitops</h1>
		<!-- TODO FuzGitopsLogo -->
		<Svg
			data={fuz_logo}
			fill="var(--color_a_5)"
			size="var(--icon_size_xl2)"
			label="a friendly blue spider facing you"
		/>
	</div>
	<p class="text_align_center">a tool for managing many repos 🪄</p>
</header>
