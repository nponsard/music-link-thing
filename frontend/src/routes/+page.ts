import { fetch_links } from '$lib';
import type { PageLoad } from './$types';

export const load: PageLoad = async () => {
	const links = await fetch_links();
	return {
		links
	};
};
