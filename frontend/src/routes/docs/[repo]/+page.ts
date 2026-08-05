import { redirect } from "@sveltejs/kit";
import type { PageLoad } from "./$types.js";

export const load: PageLoad = ({ params }) => {
	redirect(302, `/docs/${encodeURIComponent(params.repo)}/latest`);
};
