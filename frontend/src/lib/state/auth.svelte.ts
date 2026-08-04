export interface AuthUser {
  username: string;
  avatar_url: string | null;
}

class AuthState {
  user = $state<AuthUser | null>(null);
  checked = $state(false);

  async checkAuth(): Promise<void> {
    try {
      const res = await fetch('/api/v1/auth/me');
      this.user = res.ok ? await res.json() : null;
    } catch {
      this.user = null;
    } finally {
      this.checked = true;
    }
  }

  login(): void {
    window.location.href = '/auth/web/login';
  }

  async logout(): Promise<void> {
    await fetch('/api/v1/auth/logout', { method: 'POST' });
    this.user = null;
  }
}

export const auth = new AuthState();
