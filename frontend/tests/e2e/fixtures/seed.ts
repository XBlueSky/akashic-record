import type { APIRequestContext } from '@playwright/test';

const BACKEND = process.env.PLAYWRIGHT_BACKEND_URL ?? 'http://localhost:13001';

export interface SeededSession {
  apiKey: string;
  username: string;
  setCookieHeader: string; // raw Set-Cookie value (for parity assertions)
}

export interface SeededNote {
  uuid: string;
  repoName: string;
}

export async function seedSession(
  request: APIRequestContext,
  opts: { username?: string; name?: string; gitlabUserId?: number } = {},
): Promise<SeededSession> {
  const uniqueSuffix = crypto.randomUUID().slice(0, 8);
  const resp = await request.post(`${BACKEND}/test/fixtures/session`, {
    data: {
      username: opts.username ?? `e2e-${uniqueSuffix}`,
      name: opts.name ?? 'E2E User',
      gitlab_user_id: opts.gitlabUserId ?? 999000 + Math.floor(Math.random() * 1000),
    },
  });
  if (!resp.ok()) {
    throw new Error(`seedSession failed: ${resp.status()} ${await resp.text()}`);
  }
  const setCookie = resp.headers()['set-cookie'];
  if (!setCookie) throw new Error('seedSession: missing Set-Cookie header');
  const body = await resp.json();
  return {
    apiKey: body.api_key,
    username: body.username,
    setCookieHeader: setCookie,
  };
}

export async function seedNote(
  request: APIRequestContext,
  opts: { repoName?: string; title?: string; content?: string; category?: string } = {},
): Promise<SeededNote> {
  const resp = await request.post(`${BACKEND}/test/fixtures/note`, {
    data: {
      repo_name: opts.repoName ?? 'akashic-record',
      title: opts.title ?? `e2e-note-${crypto.randomUUID().slice(0, 8)}`,
      content: opts.content ?? 'e2e seeded content',
      category: opts.category ?? 'ARCHITECTURE',
    },
  });
  if (!resp.ok()) {
    throw new Error(`seedNote failed: ${resp.status()} ${await resp.text()}`);
  }
  const body = await resp.json();
  return { uuid: body.uuid, repoName: body.repo_name };
}
