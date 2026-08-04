import { redirect } from '@sveltejs/kit';

export function load({ params }: { params: { repo: string } }) {
  redirect(307, `/r/${params.repo}/timeline`);
}
