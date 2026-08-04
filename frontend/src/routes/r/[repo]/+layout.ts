import { fetchRepoDetail } from "$lib/api";
import type { RepoDetail } from "$lib/api";

export const ssr = false;

export async function load({ params }: { params: { repo: string } }) {
  // Fetch repo detail client-side (ssr=false) so the layout + child routes
  // (e.g. graph) can derive source_type without re-fetching. Swallow errors
  // so the layout still renders with a degraded header when the backend is
  // down (detail === null).
  let detail: RepoDetail | null = null;
  try {
    detail = await fetchRepoDetail(params.repo);
  } catch {
    /* backend may be down; header degrades, switcher + children still render */
  }
  return { repo: params.repo, detail };
}
