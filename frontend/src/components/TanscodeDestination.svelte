<script>
	import { backend_url } from '$lib';

	let { data } = $props();

	let backend = backend_url();
	if (backend == '') {
		backend = window.location.origin;
	}
	const ref = $derived(`${backend}/transcode/${data.id}`);
</script>

<a href={ref}>{ref}</a>
