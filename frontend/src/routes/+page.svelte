<script lang="ts">
	import { fetch_links, delete_link, add_link, fetch_link } from '$lib';
	import Link from '../components/Link.svelte';
	import TanscodeDestination from '../components/TanscodeDestination.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	let poller: any;
	const setupPoller = (id: string) => {
		if (poller) {
			clearInterval(poller);
		}
		poller = setInterval(doPoll(id), 2000);
	};

	const doPoll = (id: string) => async () => {
		console.log('polling', id);
		current_link = await fetch_link(id);

		if (current_link.error || current_link.finished) {
			clearInterval(poller);
			poller = null;
		}
	};

	// svelte-ignore state_referenced_locally
	let links = $state(data.links);
	let current_link: null | Link = $state(null);

	let url = $state('');

	async function click_add_link() {
		let l = await add_link(url);
		current_link = l;
		setupPoller(l.id);
		links = await fetch_links();
	}

	async function delete_func(id: string) {
		await delete_link(id);

		links = await fetch_links();
		console.log(links);
	}
	async function delete_current_link(id: string) {
		await delete_link(id);
		clearInterval(poller);
		poller = null;
		current_link = null;

		links = await fetch_links();
		console.log(links);
	}

	let status_text = $derived.by(() => {
		if (current_link) {
			if (current_link.error) {
				return `⚠️ error when processing file: ${current_link.error}`;
			} else if (!current_link.finished) {
				return `⏱️ processing file...`;
			} else {
				return null;
			}
		} else {
			return '';
		}
	});
</script>

<div class="m-4">
	<label>
		url:
		<input bind:value={url} />
	</label>

	<button onclick={click_add_link}>Add link</button>
</div>

{#if current_link}
	<div class="m-4">
		Link to {current_link.url}
	</div>
	{#if current_link.finished}
		<div>✅ Transcode available at: <TanscodeDestination data={current_link} /></div>
	{:else}
		<div>Status: {status_text}</div>
	{/if}
	<button onclick={delete_current_link(current_link.id)}>Delete this link</button>
{/if}

<div class="mt-5">
	<h2>Other links</h2>
	{#each links as link (link.id)}
		<Link data={link} {delete_func} />
	{/each}
</div>
