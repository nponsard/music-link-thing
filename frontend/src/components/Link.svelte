<script>
	import { backend_url } from '$lib';

	let { data, delete_func } = $props();
	const click_delete = $derived(async () => {
		await delete_func(data.id);
	});

	let backend = backend_url();
	if (backend == '/') {
		backend = window.location.origin;
	}
	const ref = $derived(`${backend}/transcode/${data.id}`);
</script>

<div>
	<button onclick={click_delete}>DELETE</button>
	<div class="inline">{data.url}</div>
	<div class="inline"><a href={ref}>{ref}</a></div>
</div>
