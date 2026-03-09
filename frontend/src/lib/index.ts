// place files you want to import through the `$lib` alias in this folder.
interface Link {
	id: string;
	url: string;
	original_hash: string;
}

export function backend_url() {
	if (process.env.NODE_ENV == 'production') {
		return '';
	} else {
		return 'http://127.0.0.1:3000';
	}
}

export async function fetch_links(): Promise<Link[]> {
	const base = backend_url();

	const result = await fetch( `${base}/api/links`);
	return await result.json();
}

export async function add_link(url: string): Promise<Link> {
	const base = backend_url();

	const result = await fetch( `${base}/api/link`, {
		method: 'POST',
		body: JSON.stringify({ url }),
		headers: {
			'Content-Type': 'application/json'
		}
	});
	return await result.json();
}

export async function delete_link(id: string): Promise<Link> {
	const base = backend_url();

	const result = await fetch(`${base}/api/link/${id}`, {
		method: 'DELETE'
	});
	return await result.json();
}
