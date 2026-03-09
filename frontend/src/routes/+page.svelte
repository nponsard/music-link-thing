<script lang="ts">
	import { add_link, fetch_links, delete_link } from '$lib';
	import Link from '../components/Link.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	// svelte-ignore state_referenced_locally
	let links = $state(data.links);

	let url = $state('');

	async function click_add_link() {
		await add_link(url);

		links = await fetch_links();
	}

	async function delete_func(id: string) {
		await delete_link(id);

		links = await fetch_links();
		console.log(links);
	}
</script>

{#each links as link (link.id)}
	<Link data={link} {delete_func} />
{/each}

<div>
	<label>
		url:
		<input bind:value={url} />
	</label>

	<button onclick={click_add_link}>Add link</button>
</div>
