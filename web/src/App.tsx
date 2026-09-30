import { useEffect, useState } from 'react';
import { api, API_BASE, getToken } from './api';

interface Task {
  id: number;
  status: string;
  source_path: string;
  number: string | null;
  dest_path: string | null;
  error: string | null;
}

interface NetdiskState {
  netdisk: {
    host: string;
    port: number;
    mount_root: string;
    url_template: string;
    path_prefix: string;
  };
  strm: { root: string; jobs: string[]; recursive: boolean; max_depth: number; bom: boolean; interval_hours: number };
  strm_root: string;
  manifest_path: string;
  mount_ok: boolean;
  out_writable: boolean;
  jobs: { dir: string; exists: boolean }[];
  source: { kind: string; base: string; user: string | null; password: string | null; path_prefix: string; timeout_secs: number };
  source_kind: string | null;
  source_root: string | null;
  source_ok: boolean;
}

interface SourceProbe {
  kind: string;
  root: string;
  dir: string;
  total: number;
  sample: { path: string; name: string; size: number | null; cloud_path: string | null; url: string | null }[];
}

interface StrmStatus {
  running: boolean;
  last_run: number | null;
  next_run: number | null;
  interval_hours: number;
  manifest_entries: number;
  generated: number;
  out_root: string;
}

interface VideoMeta {
  number: string;
  title: string | null;
  actors: string[];
  release_date: string | null;
  studio: string | null;
  director: string | null;
  runtime_min: number | null;
  tags: string[];
  cover_url: string | null;
  website: string | null;
  source: string | null;
  uncensored: boolean | null;
}

interface Candidate {
  provider: string;
  label: string;
  status: 'hit' | 'skipped' | 'failed';
  error: string | null;
  meta: VideoMeta | null;
}

interface ManualItem {
  number: string;
  title: string | null;
}

interface StrmStats {
  total: number;
  added: number;
  skipped: number;
  failed: number;
  out_root: string;
  errors: string[];
}

interface StrmScan {
  total: number;
  unparseable: number;
  out_root: string;
  preview: { source: string; number: string | null; url: string; url_ok: boolean }[];
}

interface ProxyCfg {
  enabled: boolean;
  kernel: string;
  subscribe_url: string | null;
  kernel_path: string | null;
  port: number;
  expose_lan: boolean;
  external_proxy: string | null;
}

// Phase 的 serde 形式（后端 rename_all = snake_case）：
// 无参变体是裸字符串（"disabled"），带参变体是 { 变体名: {...} }（{ "failed": {...} }）
type ProxyPhase =
  | 'disabled'
  | 'starting'
  | { running: { port: number } }
  | { failed: { reason: string } };

interface ProxyStatus {
  enabled: boolean;
  kernel: string;
  port: number;
  proxy: ProxyCfg;
  phase: ProxyPhase;
  effective_proxy: string | null;
  kernel_found: boolean;
}

// ---------- 自带影视库（P4） ----------
interface LibItem {
  number: string;
  title: string;
  year: string;
  premiered: string;
  actors: string[];
  tags: string[];
  studio: string | null;
  runtime_min: number | null;
  has_poster: boolean;
  file_count: number;
}

interface LibFile {
  name: string;
  url: string;
}

interface LibDetail extends Omit<LibItem, 'has_poster' | 'file_count'> {
  poster: string | null;
  files: LibFile[];
}

// `<img>` / `<video>` 标签发不出 Authorization 头，海报与播放走 ?t= 查询串
function mediaQuery(): string {
  const t = getToken();
  return t ? `?t=${encodeURIComponent(t)}` : '';
}

// 全局消息横幅的配色分类：错误红 / 进行中琥珀 / 其余绿
const bannerKind = (m: string): 'ok' | 'err' | 'info' => {
  if (/error|失败|错误|不可用|不可写|算不出|拒绝|denied|failed/i.test(m)) return 'err';
  if (/^正在|正在|稍候|稍后/.test(m)) return 'info';
  return 'ok';
};

function App() {
  const [version, setVersion] = useState('');
  const [filename, setFilename] = useState('');
  const [parsed, setParsed] = useState<Record<string, unknown> | null>(null);
  const [dir, setDir] = useState('');
  const [tasks, setTasks] = useState<Task[]>([]);
  const [msg, setMsg] = useState('');

  const [nd, setNd] = useState<NetdiskState | null>(null);
  const [host, setHost] = useState('');
  const [port, setPort] = useState('19798');
  const [mountRoot, setMountRoot] = useState('');
  const [prefix, setPrefix] = useState('');
  const [strmRoot, setStrmRoot] = useState('');
  const [jobsText, setJobsText] = useState('');
  const [intervalH, setIntervalH] = useState('0');
  const [status, setStatus] = useState<StrmStatus | null>(null);
  const [stats, setStats] = useState<StrmStats | null>(null);
  const [scan, setScan] = useState<StrmScan | null>(null);

  // 目录源：网盘是挂成本机目录（local）还是走 WebDAV（webdav —— 安卓唯一可行）
  const [srcKind, setSrcKind] = useState('local');
  const [srcBase, setSrcBase] = useState('http://127.0.0.1:19798/dav');
  const [srcUser, setSrcUser] = useState('');
  const [srcPass, setSrcPass] = useState('');
  const [srcPrefix, setSrcPrefix] = useState('');
  const [srcTimeout, setSrcTimeout] = useState('15');
  const [probe, setProbe] = useState<SourceProbe | null>(null);
  const [probeMsg, setProbeMsg] = useState('');

  const [px, setPx] = useState<ProxyStatus | null>(null);
  const [pxLog, setPxLog] = useState('');

  // 内置 CD2 引擎（APK 壳拉起，经 /api/cd2/status 代探 —— CD2 API 无 CORS）
  const [cd2, setCd2] = useState<{ alive: boolean; url: string; dav: string } | null>(null);
  const [cd2Panel, setCd2Panel] = useState(false);

  const [candNumber, setCandNumber] = useState('');
  const [candidates, setCandidates] = useState<Candidate[] | null>(null);
  const [manualList, setManualList] = useState<ManualItem[]>([]);
  const [candMsg, setCandMsg] = useState('');

  // ---------- 影视库（默认视图） ----------
  const [view, setView] = useState<'lib' | 'cfg'>('lib');
  const [libQuery, setLibQuery] = useState('');
  const [libItems, setLibItems] = useState<LibItem[] | null>(null);
  const [libTotal, setLibTotal] = useState(0);
  const [libLoading, setLibLoading] = useState(false);
  const [detail, setDetail] = useState<LibDetail | null>(null);
  const [playing, setPlaying] = useState<{ number: string; title: string; file: number } | null>(null);

  const loadLib = (query = libQuery) => {
    setLibLoading(true);
    api<{ total: number; items: LibItem[] }>(
      `/api/library?query=${encodeURIComponent(query)}`,
    )
      .then((j) => {
        setLibItems(j.items);
        setLibTotal(j.total);
        setLibLoading(false);
      })
      .catch(() => setLibLoading(false));
  };

  const openDetail = (number: string) => {
    api<LibDetail>(`/api/library/${encodeURIComponent(number)}`)
      .then(setDetail)
      .catch((e) => setMsg(String(e)));
  };

  const play = (number: string, title: string, file: number) => {
    setPlaying({ number, title, file });
  };

  const refresh = () => api<Task[]>('/api/tasks').then(setTasks).catch(() => {});

  const loadStatus = () =>
    api<StrmStatus>('/api/strm/status').then(setStatus).catch(() => {});

  const loadManual = () =>
    api<{ items: ManualItem[] }>('/api/videos/manual')
      .then((j) => setManualList(j.items))
      .catch(() => {});

  const loadCandidates = () => {
    if (!candNumber.trim()) return;
    setCandMsg('正在向各源查询…');
    api<{ candidates: Candidate[] }>('/api/scrape/candidates', {
      method: 'POST',
      body: JSON.stringify({ number: candNumber.trim() }),
    })
      .then((j) => { setCandidates(j.candidates); setCandMsg(''); })
      .catch((e) => setCandMsg(String(e)));
  };

  const useCandidate = (m: VideoMeta) => {
    api(`/api/videos/${encodeURIComponent(m.number)}/meta`, {
      method: 'PUT',
      body: JSON.stringify({ meta: m }),
    })
      .then(() => { setCandMsg(`已保存 ${m.number} 的人工结果（之后处理会直接用它，不再刮削）`); loadManual(); })
      .catch((e) => setCandMsg(String(e)));
  };

  const clearManual = (number: string) => {
    api(`/api/videos/${encodeURIComponent(number)}/meta`, { method: 'DELETE' })
      .then(() => { setCandMsg(`已取消 ${number} 的人工结果（恢复自动刮削）`); loadManual(); })
      .catch((e) => setCandMsg(String(e)));
  };

  const loadNetdisk = () =>
    api<NetdiskState>('/api/netdisk')
      .then((s) => {
        setNd(s);
        setHost(s.netdisk.host);
        setPort(String(s.netdisk.port));
        setMountRoot(s.netdisk.mount_root);
        setPrefix(s.netdisk.path_prefix);
        setStrmRoot(s.strm.root);
        setJobsText(s.strm.jobs.join('\n'));
        setIntervalH(String(s.strm.interval_hours));
        setSrcKind(s.source.kind || 'local');
        setSrcBase(s.source.base || 'http://127.0.0.1:19798/dav');
        setSrcUser(s.source.user || '');
        setSrcPass(s.source.password || '');
        setSrcPrefix(s.source.path_prefix || '');
        setSrcTimeout(String(s.source.timeout_secs || 15));
      })
      .catch(() => {});

  const probeSource = () => {
    setProbeMsg('正在试连…');
    api<SourceProbe>('/api/source/probe', { method: 'POST', body: JSON.stringify({ dir: '' }) })
      .then((j) => { setProbe(j); setProbeMsg(''); })
      .catch((e) => { setProbe(null); setProbeMsg(String(e)); });
  };

  // 版本号：引擎是异步拉起的（APK 上前端先起、后端后起），
  // 拉不到就每 2s 重试，最多 30 次 —— 修掉旧版「启动太早永远显示 v?」的问题。
  useEffect(() => {
    let alive = true;
    let tries = 0;
    let timer: ReturnType<typeof setTimeout>;
    const tick = () => {
      api<{ version: string }>('/api/health')
        .then((j) => { if (alive) setVersion(j.version); })
        .catch(() => {
          if (!alive) return;
          tries += 1;
          if (tries <= 30) timer = setTimeout(tick, 2000);
        });
    };
    tick();
    return () => { alive = false; clearTimeout(timer); };
  }, []);

  useEffect(() => {
    refresh();
    loadNetdisk();
    loadStatus();
    loadManual();
    loadProxy();
    loadLib();
    loadCd2();
    // 定时器状态会自己变（后台在跑），轮询刷新
    const t = setInterval(loadStatus, 10000);
    // 内核是异步拉起的（拉订阅 + 等端口），启动中要多刷几次才能看到结果
    const t2 = setInterval(loadProxy, 4000);
    // CD2 引擎首次要建库，几秒后才就绪 —— 轮询直到 alive 为止
    const t3 = setInterval(() => {
      setCd2((cur) => {
        if (cur?.alive) return cur; // 已就绪就不再打（setInterval 回调里判断后由上层 clearInterval）
        loadCd2();
        return cur;
      });
    }, 6000);
    return () => {
      clearInterval(t);
      clearInterval(t2);
      clearInterval(t3);
    };
  }, []);

  // 切回影视库视图时刷新（刚跑完一轮刮削回来看新海报）
  const switchView = (v: 'lib' | 'cfg') => {
    setView(v);
    if (v === 'lib') loadLib();
  };

  const saveNetdisk = () => {
    if (!nd) return;
    const body = {
      netdisk: { ...nd.netdisk, host, port: Number(port) || 19798, mount_root: mountRoot, path_prefix: prefix },
      strm: {
        ...nd.strm,
        root: strmRoot,
        interval_hours: Math.max(0, Number(intervalH) || 0),
        jobs: jobsText.split('\n').map((s) => s.trim()).filter(Boolean),
      },
      source: {
        ...nd.source,
        kind: srcKind,
        base: srcBase,
        user: srcUser || null,
        password: srcPass || null,
        path_prefix: srcPrefix,
        timeout_secs: Math.max(5, Number(srcTimeout) || 15),
      },
    };
    api('/api/netdisk', { method: 'PUT', body: JSON.stringify(body) })
      .then(() => { setMsg('网盘配置已保存'); loadNetdisk(); loadStatus(); })
      .catch((e) => setMsg(String(e)));
  };

  const runStrm = (force: boolean) => {
    setMsg('正在扫描网盘并刮削…（首次会比较慢）');
    api<StrmStats>('/api/strm/run', { method: 'POST', body: JSON.stringify({ force }) })
      .then((s) => { setStats(s); setMsg(''); refresh(); loadStatus(); })
      .catch((e) => { setMsg(String(e)); loadStatus(); });
  };

  const loadProxy = () => api<ProxyStatus>('/api/proxy/status').then(setPx).catch(() => {});

  const saveProxy = (enabled?: boolean) => {
    if (!px) return;
    const body = { proxy: { ...px.proxy, enabled: enabled ?? px.proxy.enabled } };
    api('/api/proxy/config', { method: 'PUT', body: JSON.stringify(body) })
      .then(() => {
        setMsg(enabled === false ? '已停用内置内核（回落外部代理/直连）' : '代理配置已保存，正在后台拉起内核…');
        setTimeout(loadProxy, 900);
      })
      .catch((e) => setMsg(String(e)));
  };

  const reloadProxy = () =>
    api('/api/proxy/reload', { method: 'POST' })
      .then(() => { setMsg('正在重新拉起内核…'); setTimeout(loadProxy, 1500); })
      .catch((e) => setMsg(String(e)));

  const loadCd2 = () =>
    api<{ alive: boolean; url: string; dav: string }>('/api/cd2/status')
      .then(setCd2)
      .catch(() => setCd2(null));

  const showPxLog = () =>
    api<{ log: string }>('/api/proxy/log?lines=80')
      .then((j) => setPxLog(j.log || '（内核日志为空）'))
      .catch((e) => setMsg(String(e)));

  const phaseText = (p: ProxyPhase): string => {
    // 后端 serde 用了 snake_case，无参变体是小写裸字符串
    if (p === 'disabled') return '未启用';
    if (p === 'starting') return '正在启动…';
    if (typeof p === 'object' && 'running' in p) return `运行中 · 端口 ${p.running.port}`;
    if (typeof p === 'object' && 'failed' in p) return `失败：${p.failed.reason}`;
    return String(p);
  };

  const phasePill = (p: ProxyPhase): string => {
    if (typeof p === 'object' && 'running' in p) return 'pill ok';
    if (typeof p === 'object' && 'failed' in p) return 'pill err';
    if (p === 'starting') return 'pill warn';
    return 'pill dim';
  };

  const fmtTime = (t: number | null) =>
    t ? new Date(t * 1000).toLocaleString() : '—';

  const candPill = (s: Candidate['status']): string =>
    s === 'hit' ? 'pill ok' : s === 'failed' ? 'pill err' : 'pill dim';
  const candPillText = (s: Candidate['status']): string =>
    s === 'hit' ? '命中' : s === 'skipped' ? '不处理此形态' : '未命中';

  return (
    <>
      <header>
        <div className="brand">
          <h1>MDC-RS {version && <span className="ver">v{version}</span>}</h1>
          <div className="sub">115 网盘刮削 · .strm 生成 · 内置代理内核</div>
        </div>
      </header>

      <div className="wrap">
        {msg && <div className={`banner on ${bannerKind(msg)}`}>{msg}</div>}

        <div className="vseg">
          <button className={view === 'lib' ? 'on' : ''} onClick={() => switchView('lib')}>影视库</button>
          <button className={view === 'cfg' ? 'on' : ''} onClick={() => switchView('cfg')}>设置</button>
        </div>

        {view === 'lib' && (
          <>
            <div className="row" style={{ marginTop: 16 }}>
              <input className="inline-input" style={{ flex: 1, minWidth: 160 }}
                placeholder="搜番号 / 标题 / 演员"
                value={libQuery} onChange={(e) => setLibQuery(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') loadLib(); }} />
              <button className="btn pri" onClick={() => loadLib()}>搜索</button>
            </div>
            <div className="countbar">
              <span className="hint" style={{ margin: 0 }}>
                共 {libTotal} 部
                {libItems && libTotal !== libItems.length ? ` · 筛出 ${libItems.length}` : ''}
                {libLoading ? ' · 读取中…' : ''}
              </span>
              <span className="sp" />
              <button className="tbtn" onClick={() => loadLib()}>刷新</button>
            </div>

            {libItems && libItems.length === 0 ? (
              <div className="empty">
                库还是空的 —— 去「设置」的网盘刮削页配好 CD2、跑一轮就有了。
              </div>
            ) : (
              <div className="wall">
                {(libItems ?? []).map((it) => (
                  <button key={it.number} className="wcard" onClick={() => openDetail(it.number)}>
                    <div className="pwrap">
                      {it.has_poster
                        ? <img loading="lazy" alt=""
                            src={`${API_BASE}/api/library/${encodeURIComponent(it.number)}/poster${mediaQuery()}`} />
                        : <div className="ph">{it.number}</div>}
                    </div>
                    <div className="wname">{it.number}</div>
                  </button>
                ))}
              </div>
            )}
          </>
        )}

        {view === 'cfg' && (
          <>
        <section className="card">
          <div className="thead">番号解析测试</div>
          <p className="hint">输入一个文件名，看看引擎从里面认出了什么（番号、前后缀、清晰度…）。</p>
          <div className="row" style={{ marginTop: 0 }}>
            <input className="inline-input" style={{ flex: 1, minWidth: 200 }} value={filename}
              placeholder="例如: [FANZA] MIDV-567 1080p.mp4"
              onChange={(e) => setFilename(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter')
                  api<Record<string, unknown>>('/api/parse', { method: 'POST', body: JSON.stringify({ filename }) })
                    .then(setParsed).catch((e) => setMsg(String(e)));
              }} />
            <button className="btn pri" onClick={() =>
              api<Record<string, unknown>>('/api/parse', { method: 'POST', body: JSON.stringify({ filename }) })
                .then(setParsed).catch((e) => setMsg(String(e)))
            }>解析</button>
          </div>
          {parsed && <pre>{JSON.stringify(parsed, null, 2)}</pre>}
        </section>

        <section className="card">
          <div className="thead">内置 CD2 引擎 {cd2 && <span className={`tag ${cd2.alive ? 'ok' : 'warn'}`}>{cd2.alive ? '运行中' : '启动中…'}</span>}</div>
          <p className="hint">
            APK 里带着 CloudDrive2 官方安卓引擎（端口 <code>127.0.0.1:19798</code>）。
            第一次用：先在下面的<b>管理页</b>登录 CD2 账号并挂载 115，然后回到「目录源」选
            <b>WebDAV</b>，基址填 <code>{cd2?.dav ?? 'http://127.0.0.1:19798/dav'}</code>，
            账号密码用同一组 CD2 账号 —— 就能直接刮网盘里的视频。
            引擎首次启动要建库，等十几秒属正常。
          </p>
          {cd2?.alive && (
            <div className="row">
              <button className="btn pri" onClick={() => setCd2Panel((v) => !v)}>
                {cd2Panel ? '收起管理页' : '打开 CD2 管理页'}
              </button>
              <button className="btn" onClick={loadCd2}>刷新状态</button>
            </div>
          )}
          {cd2Panel && cd2?.alive && (
            <iframe
              className="cd2frame"
              src={cd2.url}
              title="CloudDrive2 管理页"
            />
          )}
          {!cd2?.alive && (
            <p className="hint st-warn">引擎还没就绪（或本端不是 APK 没有内置引擎）—— 桌面/Docker 用户请自行部署 CloudDrive2 后把目录源指向它的 WebDAV。</p>
          )}
        </section>

        <section className="card">
          <div className="thead">内置代理内核 {px && <span className={phasePill(px.phase)}>{phaseText(px.phase)}</span>}</div>
          <p className="hint">
            刮削站在国内直连不通，所以软件自带内核：填你自己的订阅链接，由软件拉起内核，
            刮削与海报下载全走它，而<b>内网（CD2 / Emby / NAS）永远直连</b>，不会被代理劫持。
            内核二进制不随源码分发 —— 找不到就按提示放一个，或改用「外部代理」。
          </p>

          {px && (
            <>
              <p className="kv">
                实际出网：<code>{px.effective_proxy ?? '直连（无代理）'}</code>
              </p>
              <p className="kv" style={{ marginTop: -4 }}>
                内核二进制：
                <span className={`pill ${px.kernel_found ? 'ok' : 'err'}`}>{px.kernel_found ? '已找到' : '未找到'}</span>
              </p>

              {!px.kernel_found && (
                <p className="hint st-err">
                  把 <code>mihomo</code> 放到程序同目录，或在下面填「内核路径」。
                  没有内核时请先关闭开关、改填「外部代理」。
                </p>
              )}

              <div className="grid">
                <label className="lbl wide">订阅链接（只填你自己的，软件不预置任何节点）
                  <input value={px.proxy.subscribe_url ?? ''} placeholder="https://..."
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, subscribe_url: e.target.value || null } })} />
                </label>
                <label className="lbl">内核
                  <select value={px.proxy.kernel}
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, kernel: e.target.value } })}>
                    <option value="mihomo">mihomo（Clash.Meta，推荐）</option>
                    <option value="sing-box">sing-box（尚未实现）</option>
                  </select>
                </label>
                <label className="lbl">监听端口
                  <input type="number" value={px.proxy.port}
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, port: Number(e.target.value) || 17890 } })} />
                </label>
                <label className="lbl">外部代理（内核没起来时回落）
                  <input value={px.proxy.external_proxy ?? ''} placeholder="http://127.0.0.1:7890"
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, external_proxy: e.target.value || null } })} />
                </label>
                <label className="lbl">内核路径（留空自动探测）
                  <input value={px.proxy.kernel_path ?? ''} placeholder="C:\...\mihomo.exe"
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, kernel_path: e.target.value || null } })} />
                </label>
                <label className="lbl check wide">
                  <input type="checkbox" checked={px.proxy.expose_lan}
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, expose_lan: e.target.checked } })} />
                  放开到局域网（让 Emby / CD2 共用；默认只本机）
                </label>
                <label className="lbl check wide">
                  <input type="checkbox" checked={px.proxy.enabled}
                    onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, enabled: e.target.checked } })} />
                  启用内置内核
                </label>
              </div>
            </>
          )}

          <div className="row">
            <button className="btn pri" onClick={() => saveProxy(true)}>保存并启动</button>
            <button className="btn" onClick={() => saveProxy()}>保存</button>
            <button className="btn danger" onClick={() => saveProxy(false)}>停用</button>
            <button className="btn" onClick={reloadProxy}>重新拉起</button>
            <button className="btn" onClick={showPxLog}>看内核日志</button>
          </div>
          {pxLog && <pre>{pxLog}</pre>}
        </section>

        <section className="card">
          <div className="thead">目录源 {nd && <span className={`tag ${nd.source_ok ? 'ok' : 'err'}`}>{nd.source_ok ? (nd.source_kind ?? 'ok') : '配置不完整'}</span>}</div>
          <p className="hint">
            桌面 / Docker / NAS 上 CloudDrive2 能把 115 挂成<b>本机目录</b>，选「本地挂载」即可；
            <b>安卓没有挂载</b>（系统不给 FUSE），只能走 CD2 的 WebDAV —— 选「WebDAV」，
            此时「要监控的网盘目录」填<b>网盘内路径</b>（如 <code>/115/看剧</code>）。
          </p>

          <div className="grid">
            <label className="lbl wide">目录源类型
              <select value={srcKind} onChange={(e) => setSrcKind(e.target.value)}>
                <option value="local">本地挂载（用下面的 CD2 挂载根）</option>
                <option value="webdav">WebDAV（CD2 /dav，安卓必选）</option>
              </select>
            </label>
            {srcKind === 'webdav' && (
              <>
                <label className="lbl">WebDAV 基址
                  <input value={srcBase} onChange={(e) => setSrcBase(e.target.value)}
                    placeholder="http://127.0.0.1:19798/dav" />
                </label>
                <label className="lbl">用户名（可留空）
                  <input value={srcUser} onChange={(e) => setSrcUser(e.target.value)} />
                </label>
                <label className="lbl">密码（可留空）
                  <input type="password" value={srcPass} onChange={(e) => setSrcPass(e.target.value)} />
                </label>
                <label className="lbl">网盘路径前缀（一般留空）
                  <input value={srcPrefix} onChange={(e) => setSrcPrefix(e.target.value)}
                    placeholder="115open" />
                </label>
                <label className="lbl">超时（秒）
                  <input value={srcTimeout} onChange={(e) => setSrcTimeout(e.target.value)} />
                </label>
              </>
            )}
          </div>

          {nd && (
            <p className="kv">
              当前源：<b className={nd.source_ok ? 'st-ok' : 'st-err'}>{nd.source_ok ? nd.source_kind : '配置不完整'}</b>
              {nd.source_root ? <> · <code>{nd.source_root}</code></> : null}
              {srcKind === 'webdav' && <> · 内网请求<b>不走代理</b></>}
            </p>
          )}

          <div className="row">
            <button className="btn pri" onClick={saveNetdisk}>保存</button>
            <button className="btn" onClick={probeSource}>试连（列出源根）</button>
          </div>

          {probeMsg && <p className="hint st-err" style={{ marginTop: 10 }}>{probeMsg}</p>}
          {probe && (
            <>
              <p className="kv">
                <code>{probe.dir}</code> 下扫到 <b>{probe.total}</b> 个视频（试连只看一层，前 10 条）
              </p>
              <div className="tblwrap">
                <table className="tbl">
                  <thead><tr><th>文件</th><th>网盘路径</th><th>直链</th></tr></thead>
                  <tbody>
                    {probe.sample.map((p, i) => (
                      <tr key={i}>
                        <td className="ell">{p.name}</td>
                        <td className="ell">{p.cloud_path ?? '—'}</td>
                        <td className={`ell ${p.url ? 'st-ok' : 'st-err'}`}>{p.url ?? '算不出'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </section>

        <section className="card">
          <div className="thead">网盘刮削 {nd && <span className={`tag ${nd.mount_ok && nd.out_writable ? 'ok' : 'err'}`}>{nd.mount_ok ? '挂载正常' : '挂载不可用'}</span>}</div>
          <p className="hint">
            视频一个字节都不搬：只往输出目录写几十字节的 .strm 指针 + NFO + 海报，
            Emby / Jellyfin 扫这个目录即可。已生成的会按增量清单跳过，不重刮、不打网盘。
          </p>

          {nd && (
            <p className="kv">
              挂载根：<b className={nd.mount_ok ? 'st-ok' : 'st-err'}>{nd.mount_ok ? '已挂载' : '不可用'}</b>
              {' · '}输出目录：<b className={nd.out_writable ? 'st-ok' : 'st-err'}>{nd.out_writable ? '可写' : '不可写'}</b>
              <br />落点：<code>{nd.strm_root}</code>
            </p>
          )}

          <div className="grid">
            <label className="lbl">CD2 地址
              <input value={host} onChange={(e) => setHost(e.target.value)} placeholder="192.168.1.15" />
            </label>
            <label className="lbl">端口
              <input value={port} onChange={(e) => setPort(e.target.value)} />
            </label>
            <label className="lbl">CD2 挂载根（本机绝对路径）
              <input value={mountRoot} onChange={(e) => setMountRoot(e.target.value)} placeholder="/mnt/clouddrive" />
            </label>
            <label className="lbl">网盘路径前缀（可留空）
              <input value={prefix} onChange={(e) => setPrefix(e.target.value)} placeholder="115" />
            </label>
            <label className="lbl wide">.strm 输出目录（Emby 扫这里）
              <input value={strmRoot} onChange={(e) => setStrmRoot(e.target.value)} placeholder="留空 = <数据目录>/strm" />
            </label>
            <label className="lbl">自动运行间隔（小时，0 = 只手动）
              <input value={intervalH} onChange={(e) => setIntervalH(e.target.value)} placeholder="0" />
            </label>
            <label className="lbl wide">要监控的网盘目录（一行一个）
              <textarea className="mono" value={jobsText} onChange={(e) => setJobsText(e.target.value)}
                placeholder={'/mnt/clouddrive/115/看剧\n/mnt/clouddrive/115/新作'} />
            </label>
          </div>

          {status && (
            <p className="kv">
              {status.running && <span className="pill warn" style={{ marginRight: 6 }}>正在运行</span>}
              已生成 <b>{status.generated}</b> 个（清单记录 {status.manifest_entries} 条）
              {' · '}上次运行 {fmtTime(status.last_run)}
              {status.interval_hours > 0
                ? <> {' · '}下次 {fmtTime(status.next_run)}</>
                : <> {' · '}未开启定时</>}
            </p>
          )}

          {nd && nd.jobs.length > 0 && (
            <p className="kv" style={{ color: 'var(--dim)', fontSize: 12 }}>
              {nd.jobs.map((j, i) => (
                <span key={i} style={{ marginRight: 12 }}>
                  <span className={`pill ${j.exists ? 'ok' : 'err'}`}>{j.exists ? '✓' : '✗'}</span> <code>{j.dir}</code>
                </span>
              ))}
            </p>
          )}

          <div className="row">
            <button className="btn pri" onClick={() => runStrm(false)}>运行一轮</button>
            <button className="btn" onClick={saveNetdisk}>保存</button>
            <button className="btn" onClick={() =>
              api<StrmScan>('/api/strm/scan', { method: 'POST', body: '{}' })
                .then((s) => { setScan(s); setMsg(''); })
                .catch((e) => setMsg(String(e)))
            }>扫描预览</button>
            <button className="btn danger" onClick={() => runStrm(true)}>强制全量重写</button>
          </div>

          {scan && (
            <>
              <p className="kv">
                共 <b>{scan.total}</b> 个可识别视频{scan.unparseable > 0 && `，另有 ${scan.unparseable} 个文件名识别不出番号`}
              </p>
              <div className="tblwrap">
                <table className="tbl">
                  <thead><tr><th>番号</th><th>源文件</th><th>直链</th></tr></thead>
                  <tbody>
                    {scan.preview.map((p, i) => (
                      <tr key={i}>
                        <td style={{ whiteSpace: 'nowrap' }}>{p.number}</td>
                        <td className="ell">{p.source}</td>
                        <td className={`ell ${p.url_ok ? 'st-ok' : 'st-err'}`}>
                          {p.url_ok ? p.url : '算不出（不在挂载根下）'}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}

          {stats && (
            <p className="kv">
              共 {stats.total} · <b className="st-ok">新增 {stats.added}</b>
              {' · '}增量跳过 {stats.skipped}
              {stats.failed > 0 && <b className="st-err"> · 失败 {stats.failed}</b>}
              {stats.errors.length > 0 && <pre>{stats.errors.join('\n')}</pre>}
            </p>
          )}
        </section>

        <section className="card">
          <div className="thead">多源人工精选 {manualList.length > 0 && <span className="tag">{manualList.length} 条已保存</span>}</div>
          <p className="hint">
            各源结果<b>不合并</b>、原样列出，你挑一条。挑过之后这个番号处理时直接用它、
            不再刮削 —— 源站全挂或谁都搜不到时，这样也能出片。
          </p>
          <div className="row" style={{ marginTop: 0 }}>
            <input className="inline-input" style={{ flex: 1, minWidth: 200 }} value={candNumber}
              placeholder="番号，如 MIDV-567 / FC2-PPV-4680562 / 080918_002"
              onChange={(e) => setCandNumber(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') loadCandidates(); }} />
            <button className="btn pri" onClick={loadCandidates}>拉取各源结果</button>
          </div>

          {candMsg && <p className="hint" style={{ marginTop: 10 }}>{candMsg}</p>}

          {candidates && candidates.map((c) => (
            <div key={c.provider} className="cand">
              <div className="chead">
                <span className="cl">{c.label}</span>
                <span className="prov">{c.provider}</span>
                <span className={candPill(c.status)}>{candPillText(c.status)}</span>
                {c.status === 'hit' && c.meta && (
                  <button className="btn sm pri take" onClick={() => useCandidate(c.meta!)}>用这条</button>
                )}
              </div>

              {c.status === 'failed' && c.error && <pre>{c.error}</pre>}
              {c.status === 'hit' && c.meta && (
                <div className="cbody">
                  {c.meta.cover_url && (
                    <img className="cover" src={c.meta.cover_url} alt="" referrerPolicy="no-referrer" />
                  )}
                  <div className="cmeta">
                    <div style={{ wordBreak: 'break-word', fontWeight: 500 }}>{c.meta.title ?? '（无标题）'}</div>
                    <div className="dim2">
                      {c.meta.actors.length > 0 && <>演员：{c.meta.actors.join('、')}　</>}
                      {c.meta.runtime_min != null && <>{c.meta.runtime_min} 分钟　</>}
                      {c.meta.release_date && <>{c.meta.release_date}　</>}
                      {c.meta.studio && <>制作商：{c.meta.studio}　</>}
                      {c.meta.uncensored && <>无码　</>}
                    </div>
                    {c.meta.tags.length > 0 && (
                      <div className="dim2">标签：{c.meta.tags.join('、')}</div>
                    )}
                    {c.meta.website && (
                      <div className="site">{c.meta.website}</div>
                    )}
                  </div>
                </div>
              )}
            </div>
          ))}

          {manualList.length > 0 && (
            <>
              <div className="mhead"><b>已保存的人工精选（{manualList.length}）</b> —— 这些番号处理时不再刮削</div>
              {manualList.map((m) => (
                <div key={m.number} className="mrow">
                  <span className="mnum">{m.number}</span>
                  <span className="mttl">{m.title ?? '-'}</span>
                  <button className="tbtn danger" onClick={() => clearManual(m.number)}>取消</button>
                </div>
              ))}
            </>
          )}
        </section>

        <section className="card">
          <div className="thead">扫描目录（本地）</div>
          <div className="row" style={{ marginTop: 0 }}>
            <input className="inline-input" style={{ flex: 1, minWidth: 200 }} value={dir} placeholder="视频目录绝对路径"
              onChange={(e) => setDir(e.target.value)} />
            <button className="btn pri" onClick={() =>
              api<{ created: number }>('/api/tasks', { method: 'POST', body: JSON.stringify({ dir }) })
                .then((j) => { setMsg(`创建 ${j.created} 条任务`); refresh(); })
                .catch((e) => setMsg(String(e)))
            }>创建任务</button>
            <button className="btn" onClick={() =>
              api<{ processed: number }>('/api/tasks/run', { method: 'POST', body: JSON.stringify({ mode: 'hard_link' }) })
                .then((j) => { setMsg(`处理 ${j.processed} 条`); refresh(); })
                .catch((e) => setMsg(String(e)))
            }>运行（硬链整理）</button>
          </div>
        </section>

        <section className="card">
          <div className="thead">任务列表 {tasks.length > 0 && <span className="tag">{tasks.length}</span>}</div>
          {tasks.length === 0 ? (
            <div className="empty">还没有任务 —— 在上面填个目录、点「创建任务」。</div>
          ) : (
            <div className="tblwrap">
              <table className="tbl">
                <thead><tr><th>ID</th><th>番号</th><th>状态</th><th>源文件</th><th>错误</th></tr></thead>
                <tbody>
                  {tasks.map((t) => (
                    <tr key={t.id}>
                      <td>{t.id}</td><td>{t.number ?? '-'}</td><td>{t.status}</td>
                      <td className="ell">{t.source_path}</td>
                      <td className="st-err" style={{ maxWidth: 220, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{t.error ?? ''}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>

          </>
        )}

        <footer>视频不搬动，只生成 .strm 指针 —— 网盘里的文件留在网盘里。</footer>
      </div>

      {detail && (
        <div className="sheet" onClick={() => setDetail(null)}>
          <div className="sbox" onClick={(e) => e.stopPropagation()}>
            <div className="shead">
              <div className="pwrap sposter">
                {detail.poster
                  ? <img alt=""
                      src={`${API_BASE}/api/library/${encodeURIComponent(detail.number)}/poster${mediaQuery()}`} />
                  : <div className="ph">{detail.number}</div>}
              </div>
              <div className="sinfo">
                <div className="stitle">{detail.title}</div>
                <div className="smeta">
                  {detail.number} · {detail.premiered || detail.year || '日期未知'}
                  {detail.runtime_min != null && <> · {detail.runtime_min} 分钟</>}<br />
                  {detail.actors.length > 0 && <>演员：{detail.actors.join('、')}<br /></>}
                  {detail.studio && <>制作：{detail.studio}<br /></>}
                  {detail.tags.length > 0 && <>标签：{detail.tags.join('、')}</>}
                </div>
              </div>
              <button className="sx" onClick={() => setDetail(null)}>✕</button>
            </div>
            <div className="sfiles">
              {detail.files.map((f, i) => (
                <div key={i} className="sfile">
                  <span className="fn">{f.name}</span>
                  <button className="btn sm pri"
                    onClick={() => play(detail.number, detail.title, i)}>播放</button>
                </div>
              ))}
            </div>
          </div>
        </div>
      )}

      {playing && (
        <div className="player">
          <div className="pbar">
            <button onClick={() => setPlaying(null)}>‹ 返回</button>
            <span className="ptitle">
              {playing.title}
              {detail && detail.files[playing.file] ? ` · ${detail.files[playing.file].name}` : ''}
            </span>
          </div>
          <video
            key={`${playing.number}-${playing.file}`}
            controls autoPlay playsInline
            src={`${API_BASE}/api/library/${encodeURIComponent(playing.number)}/play?file=${playing.file}${mediaQuery().replace('?', '&')}`}
          />
        </div>
      )}
    </>
  );
}

export default App;
