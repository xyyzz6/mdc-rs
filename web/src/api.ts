const TOKEN_KEY = 'mdc_token';

// 桌面/Docker：前端由服务端同端口伺服，相对路径即可。
// APK：WebView 加载的是内嵌前端（tauri:// 或 http://tauri.localhost 源），
// API 在进程内 127.0.0.1:9208，必须打绝对地址（服务端已放行该来源的 CORS）。
export const API_BASE =
  location.protocol === 'http:' || location.protocol === 'https:'
    ? ''
    : 'http://127.0.0.1:9208';

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(t: string) {
  localStorage.setItem(TOKEN_KEY, t);
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(init?.headers as Record<string, string>),
  };
  const token = getToken();
  if (token) headers['Authorization'] = `Bearer ${token}`;
  const res = await fetch(`${API_BASE}${path}`, { ...init, headers });
  if (res.status === 401) {
    // 未登录或 token 失效：跳到登录（极简处理）
    const login = prompt('输入访问口令（用户名:密码）');
    if (login) {
      const [username, password] = login.split(':');
      const r = await fetch(`${API_BASE}/api/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      if (r.ok) {
        const j = await r.json();
        setToken(j.token);
        return api<T>(path, init);
      }
    }
    throw new Error('unauthorized');
  }
  if (!res.ok) throw new Error(`${res.status}: ${await res.text()}`);
  return res.json() as Promise<T>;
}
