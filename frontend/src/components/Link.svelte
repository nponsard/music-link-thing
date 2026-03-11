<script>
	import TranscodeDestination from './TanscodeDestination.svelte';

	let { data, delete_func } = $props();
	const click_delete = $derived(async () => {
		await delete_func(data.id);
	});
</script>

<div>
	<button onclick={click_delete}>DELETE</button>
	<div class="inline">{data.url}</div>
	<TranscodeDestination {data} />
</div>
