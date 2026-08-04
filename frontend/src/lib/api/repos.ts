/**
 * Repository + active job API calls — shell subset ported from legacy api.ts.
 * SP4a additions: fetchRepoDetail, fetchBranches, deleteRepo, reingest.
 */

import { get, post, del } from "./http.js";
import type { Repository, ActiveJob, RepoDetail, Branch } from "../types/index.js";

export function fetchRepos(): Promise<Repository[]> {
  return get<Repository[]>("/repos");
}

export function fetchActiveJobs(): Promise<ActiveJob[]> {
  return get<ActiveJob[]>("/jobs/active");
}

export function fetchRepoDetail(repoName: string): Promise<RepoDetail> {
  return get<RepoDetail>(`/repos/${encodeURIComponent(repoName)}/detail`);
}

export function fetchBranches(repoName: string): Promise<Branch[]> {
  return get<Branch[]>(`/repos/${encodeURIComponent(repoName)}/branches`);
}

export async function reingest(
  repoName: string,
  branch?: string
): Promise<{ job_id: string; status: string }> {
  const params = branch ? `?branch=${encodeURIComponent(branch)}` : "";
  return post<{ job_id: string; status: string }>(
    `/repos/${encodeURIComponent(repoName)}/reingest${params}`,
    { idempotency: true }
  );
}

export function deleteRepo(repoName: string): Promise<void> {
  return del(`/repos/${encodeURIComponent(repoName)}`);
}
