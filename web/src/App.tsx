import { useEffect, useState } from 'react';
import { api } from './api';

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

  const [candNumber, setCandNumber] = useState('');
  const [candidates, setCandidates] = useState<Candidate[] | null>(null);
  const [manualList, setManualList] = useState<ManualItem[]>([]);
  const [candMsg, setCandMsg] = useState('');

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

  useEffect(() => {
    api<{ version: string }>('/api/health').then((j) => setVersion(j.version)).catch(() => setVersion('?'));
    refresh();
    loadNetdisk();
    loadStatus();
    loadManual();
    loadProxy();
    // 定时器状态会自己变（后台在跑），轮询刷新
    const t = setInterval(loadStatus, 10000);
    // 内核是异步拉起的（拉订阅 + 等端口），启动中要多刷几次才能看到结果
    const t2 = setInterval(loadProxy, 4000);
    return () => {
      clearInterval(t);
      clearInterval(t2);
    };
  }, []);

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

  const showPxLog = () =>
    api<{ log: string }>('/api/proxy/log?lines=80')
      .then((j) => setPxLog(j.log || '（内核日志为空）'))
      .catch((e) => setMsg(String(e)));

  const phaseText = (p: ProxyPhase): string => {
    // 后端 serde 用了 snake_case，无参变体是小写裸字符串
    if (p === 'disabled') return '未启用';
    if (p === 'starting') return '正在启动…';
    if (typeof p === 'object' && 'running' in p) return `运行中（端口 ${p.running.port}）`;
    if (typeof p === 'object' && 'failed' in p) return `失败：${p.failed.reason}`;
    return String(p);
  };

  const phaseColor = (p: ProxyPhase): string => {
    if (typeof p === 'object' && 'running' in p) return '#2a7';
    if (typeof p === 'object' && 'failed' in p) return '#c00';
    if (p === 'starting') return '#c60';
    return '#666';
  };

  const fmtTime = (t: number | null) =>
    t ? new Date(t * 1000).toLocaleString() : '—';

  return (
    <div style={{ maxWidth: 860, margin: '0 auto', padding: 24, fontFamily: 'system-ui' }}>
      <h1>MDC-RS <small style={{ color: '#888' }}>v{version}</small></h1>

      <section style={card}>
        <h3>番号解析测试</h3>
        <div style={row}>
          <input style={input} value={filename} placeholder="例如: [FANZA] MIDV-567 1080p.mp4"
            onChange={(e) => setFilename(e.target.value)} />
          <button style={btn} onClick={() =>
            api<Record<string, unknown>>('/api/parse', { method: 'POST', body: JSON.stringify({ filename }) })
              .then(setParsed).catch((e) => setMsg(String(e)))
          }>解析</button>
        </div>
        {parsed && <pre style={pre}>{JSON.stringify(parsed, null, 2)}</pre>}
      </section>

      <section style={card}>
        <h3>内置代理内核</h3>
        <p style={hint}>
          刮削站在国内直连不通，所以软件自带内核：填你自己的订阅链接，由软件拉起内核，
          刮削与海报下载全走它，而<b>内网（CD2 / Emby / NAS）永远直连</b>，不会被代理劫持。
          内核二进制不随源码分发 —— 找不到就按提示放一个，或改用「外部代理」。
        </p>

        {px && (
          <>
            <p style={{ fontSize: 13 }}>
              状态：<b style={{ color: phaseColor(px.phase) }}>{phaseText(px.phase)}</b>
              {' · '}实际出网：<code>{px.effective_proxy ?? '直连（无代理）'}</code>
              {' · '}内核二进制：
              <b style={{ color: px.kernel_found ? '#2a7' : '#c00' }}>{px.kernel_found ? '已找到' : '未找到'}</b>
            </p>

            {!px.kernel_found && (
              <p style={{ fontSize: 12, color: '#c00' }}>
                把 <code>mihomo</code> 放到程序同目录，或在下面填「内核路径」。
                没有内核时请先关闭开关、改填「外部代理」。
              </p>
            )}

            <div style={grid}>
              <label style={{ ...lbl, gridColumn: '1 / -1' }}>订阅链接（只填你自己的，软件不预置任何节点）
                <input style={input} value={px.proxy.subscribe_url ?? ''} placeholder="https://..."
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, subscribe_url: e.target.value || null } })} />
              </label>
              <label style={lbl}>内核
                <select style={input} value={px.proxy.kernel}
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, kernel: e.target.value } })}>
                  <option value="mihomo">mihomo（Clash.Meta，推荐）</option>
                  <option value="sing-box">sing-box（尚未实现）</option>
                </select>
              </label>
              <label style={lbl}>监听端口
                <input style={input} type="number" value={px.proxy.port}
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, port: Number(e.target.value) || 17890 } })} />
              </label>
              <label style={lbl}>外部代理（内核没起来时回落）
                <input style={input} value={px.proxy.external_proxy ?? ''} placeholder="http://127.0.0.1:7890"
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, external_proxy: e.target.value || null } })} />
              </label>
              <label style={lbl}>内核路径（留空自动探测）
                <input style={input} value={px.proxy.kernel_path ?? ''} placeholder="C:\...\mihomo.exe"
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, kernel_path: e.target.value || null } })} />
              </label>
              <label style={{ ...lbl, display: 'flex', alignItems: 'center', gap: 6 }}>
                <input type="checkbox" checked={px.proxy.expose_lan}
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, expose_lan: e.target.checked } })} />
                放开到局域网（让 Emby / CD2 共用；默认只本机）
              </label>
              <label style={{ ...lbl, display: 'flex', alignItems: 'center', gap: 6 }}>
                <input type="checkbox" checked={px.proxy.enabled}
                  onChange={(e) => setPx({ ...px, proxy: { ...px.proxy, enabled: e.target.checked } })} />
                启用内置内核
              </label>
            </div>
          </>
        )}

        <div style={row}>
          <button style={btn} onClick={() => saveProxy()}>保存</button>
          <button style={btn} onClick={() => saveProxy(true)}>保存并启动</button>
          <button style={btn} onClick={() => saveProxy(false)}>停用</button>
          <button style={btn} onClick={reloadProxy}>重新拉起</button>
          <button style={btn} onClick={showPxLog}>看内核日志</button>
        </div>
        {pxLog && <pre style={pre}>{pxLog}</pre>}
      </section>

      <section style={card}>
        <h3>目录源（网盘从哪读）</h3>
        <p style={hint}>
          桌面 / Docker / NAS 上 CloudDrive2 能把 115 挂成<b>本机目录</b>，选「本地挂载」即可；
          <b>安卓没有挂载</b>（系统不给 FUSE），只能走 CD2 的 WebDAV —— 选「WebDAV」，
          此时「要监控的网盘目录」填<b>网盘内路径</b>（如 <code>/115/看剧</code>）。
        </p>

        <div style={grid}>
          <label style={lbl}>目录源类型
            <select style={input} value={srcKind} onChange={(e) => setSrcKind(e.target.value)}>
              <option value="local">本地挂载（用下面的 CD2 挂载根）</option>
              <option value="webdav">WebDAV（CD2 /dav，安卓必选）</option>
            </select>
          </label>
          {srcKind === 'webdav' && (
            <>
              <label style={lbl}>WebDAV 基址
                <input style={input} value={srcBase} onChange={(e) => setSrcBase(e.target.value)}
                  placeholder="http://127.0.0.1:19798/dav" />
              </label>
              <label style={lbl}>用户名（可留空）
                <input style={input} value={srcUser} onChange={(e) => setSrcUser(e.target.value)} />
              </label>
              <label style={lbl}>密码（可留空）
                <input style={input} type="password" value={srcPass} onChange={(e) => setSrcPass(e.target.value)} />
              </label>
              <label style={lbl}>网盘路径前缀（一般留空）
                <input style={input} value={srcPrefix} onChange={(e) => setSrcPrefix(e.target.value)}
                  placeholder="115open" />
              </label>
              <label style={lbl}>超时（秒）
                <input style={input} value={srcTimeout} onChange={(e) => setSrcTimeout(e.target.value)} />
              </label>
            </>
          )}
        </div>

        {nd && (
          <p style={{ fontSize: 13 }}>
            当前源：
            <b style={{ color: nd.source_ok ? '#2a7' : '#c00' }}>{nd.source_ok ? nd.source_kind : '配置不完整'}</b>
            {nd.source_root ? <> · <code>{nd.source_root}</code></> : null}
            {srcKind === 'webdav' && <> · 内网请求<b>不走代理</b></>}
          </p>
        )}

        <div style={row}>
          <button style={btn} onClick={saveNetdisk}>保存</button>
          <button style={btn} onClick={probeSource}>试连（列出源根）</button>
        </div>

        {probeMsg && <p style={{ fontSize: 12, color: '#c00' }}>{probeMsg}</p>}
        {probe && (
          <div style={{ fontSize: 12, marginTop: 8 }}>
            <p>
              <code>{probe.dir}</code> 下扫到 <b>{probe.total}</b> 个视频（试连只看一层，前 10 条）
            </p>
            <table style={{ width: '100%', borderCollapse: 'collapse' }}>
              <thead><tr><th>文件</th><th>网盘路径</th><th>直链</th></tr></thead>
              <tbody>
                {probe.sample.map((p, i) => (
                  <tr key={i}>
                    <td style={ellipsis}>{p.name}</td>
                    <td style={ellipsis}>{p.cloud_path ?? '—'}</td>
                    <td style={{ ...ellipsis, color: p.url ? '#2a7' : '#c00' }}>{p.url ?? '算不出'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section style={card}>
        <h3>网盘刮削（CD2 挂载 → .strm）</h3>
        <p style={hint}>
          视频一个字节都不搬：只往输出目录写几十字节的 .strm 指针 + NFO + 海报，
          Emby / Jellyfin 扫这个目录即可。已生成的会按增量清单跳过，不重刮、不打网盘。
        </p>

        {nd && (
          <p style={{ fontSize: 13 }}>
            挂载根：
            <b style={{ color: nd.mount_ok ? '#2a7' : '#c00' }}>{nd.mount_ok ? '已挂载' : '不可用'}</b>
            {' · '}输出目录：
            <b style={{ color: nd.out_writable ? '#2a7' : '#c00' }}>{nd.out_writable ? '可写' : '不可写'}</b>
            {' · '}落点：<code>{nd.strm_root}</code>
          </p>
        )}

        <div style={grid}>
          <label style={lbl}>CD2 地址
            <input style={input} value={host} onChange={(e) => setHost(e.target.value)} placeholder="192.168.1.15" />
          </label>
          <label style={lbl}>端口
            <input style={input} value={port} onChange={(e) => setPort(e.target.value)} />
          </label>
          <label style={lbl}>CD2 挂载根（本机绝对路径）
            <input style={input} value={mountRoot} onChange={(e) => setMountRoot(e.target.value)} placeholder="/mnt/clouddrive" />
          </label>
          <label style={lbl}>网盘路径前缀（可留空）
            <input style={input} value={prefix} onChange={(e) => setPrefix(e.target.value)} placeholder="115" />
          </label>
          <label style={{ ...lbl, gridColumn: '1 / -1' }}>.strm 输出目录（Emby 扫这里）
            <input style={input} value={strmRoot} onChange={(e) => setStrmRoot(e.target.value)} placeholder="留空 = <数据目录>/strm" />
          </label>
          <label style={lbl}>自动运行间隔（小时，0 = 只手动）
            <input style={input} value={intervalH} onChange={(e) => setIntervalH(e.target.value)} placeholder="0" />
          </label>
          <label style={{ ...lbl, gridColumn: '1 / -1' }}>要监控的网盘目录（一行一个）
            <textarea style={{ ...input, height: 76, fontFamily: 'monospace', fontSize: 12 }}
              value={jobsText} onChange={(e) => setJobsText(e.target.value)}
              placeholder={'/mnt/clouddrive/115/看剧\n/mnt/clouddrive/115/新作'} />
          </label>
        </div>

        {status && (
          <p style={{ fontSize: 12, color: '#555' }}>
            {status.running && <b style={{ color: '#c60' }}>正在运行 · </b>}
            已生成 <b>{status.generated}</b> 个（清单记录 {status.manifest_entries} 条）
            {' · '}上次运行 {fmtTime(status.last_run)}
            {status.interval_hours > 0
              ? <> {' · '}下次 {fmtTime(status.next_run)}</>
              : <> {' · '}未开启定时</>}
          </p>
        )}

        {nd && nd.jobs.length > 0 && (
          <p style={{ fontSize: 12, color: '#888' }}>
            {nd.jobs.map((j, i) => (
              <span key={i} style={{ marginRight: 12 }}>
                {j.exists ? '✅' : '❌'} <code>{j.dir}</code>
              </span>
            ))}
          </p>
        )}

        <div style={row}>
          <button style={btn} onClick={saveNetdisk}>保存</button>
          <button style={btn} onClick={() =>
            api<StrmScan>('/api/strm/scan', { method: 'POST', body: '{}' })
              .then((s) => { setScan(s); setMsg(''); })
              .catch((e) => setMsg(String(e)))
          }>扫描预览</button>
          <button style={btn} onClick={() => runStrm(false)}>运行一轮</button>
          <button style={btn} onClick={() => runStrm(true)}>强制全量重写</button>
        </div>

        {scan && (
          <div style={{ fontSize: 12, marginTop: 10 }}>
            <p>共 <b>{scan.total}</b> 个可识别视频{scan.unparseable > 0 && `，另有 ${scan.unparseable} 个文件名识别不出番号`}</p>
            <table style={{ width: '100%', borderCollapse: 'collapse' }}>
              <thead><tr><th>番号</th><th>源文件</th><th>直链</th></tr></thead>
              <tbody>
                {scan.preview.map((p, i) => (
                  <tr key={i}>
                    <td style={{ whiteSpace: 'nowrap', paddingRight: 8 }}>{p.number}</td>
                    <td style={ellipsis}>{p.source}</td>
                    <td style={{ ...ellipsis, color: p.url_ok ? '#2a7' : '#c00' }}>
                      {p.url_ok ? p.url : '算不出（不在挂载根下）'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {stats && (
          <p style={{ fontSize: 13, marginTop: 10 }}>
            共 {stats.total} · <b style={{ color: '#2a7' }}>新增 {stats.added}</b>
            {' · '}增量跳过 {stats.skipped}
            {stats.failed > 0 && <b style={{ color: '#c00' }}> · 失败 {stats.failed}</b>}
            {stats.errors.length > 0 && <pre style={pre}>{stats.errors.join('\n')}</pre>}
          </p>
        )}
      </section>

      <section style={card}>
        <h3>多源人工精选</h3>
        <p style={hint}>
          各源结果<b>不合并</b>、原样列出，你挑一条。挑过之后这个番号处理时直接用它、
          不再刮削 —— 源站全挂或谁都搜不到时，这样也能出片。
        </p>
        <div style={row}>
          <input style={input} value={candNumber} placeholder="番号，如 MIDV-567 / FC2-PPV-4680562 / 080918_002"
            onChange={(e) => setCandNumber(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') loadCandidates(); }} />
          <button style={btn} onClick={loadCandidates}>拉取各源结果</button>
        </div>

        {candidates && (
          <div style={{ marginTop: 10 }}>
            {candidates.map((c) => (
              <div key={c.provider} style={candCard}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <b style={{ fontSize: 13 }}>{c.label}</b>
                  <code style={{ fontSize: 11, color: '#888' }}>{c.provider}</code>
                  <span style={{
                    fontSize: 11, padding: '1px 6px', borderRadius: 4,
                    background: c.status === 'hit' ? '#EAF3DE' : c.status === 'skipped' ? '#F1EFE8' : '#FCEBEB',
                    color: c.status === 'hit' ? '#173404' : c.status === 'skipped' ? '#2C2C2A' : '#501313',
                  }}>
                    {c.status === 'hit' ? '命中' : c.status === 'skipped' ? '不处理此形态' : '未命中'}
                  </span>
                  {c.status === 'hit' && c.meta && (
                    <button style={{ ...btn, padding: '2px 10px', marginLeft: 'auto' }}
                      onClick={() => useCandidate(c.meta!)}>用这条</button>
                  )}
                </div>

                {c.status === 'failed' && c.error && (
                  <pre style={{ ...pre, marginTop: 6 }}>{c.error}</pre>
                )}
                {c.status === 'hit' && c.meta && (
                  <div style={{ display: 'flex', gap: 10, marginTop: 8 }}>
                    {c.meta.cover_url && (
                      <img src={c.meta.cover_url} alt="" referrerPolicy="no-referrer"
                        style={{ width: 66, height: 99, objectFit: 'cover', borderRadius: 4, flex: 'none', background: '#eee' }} />
                    )}
                    <div style={{ fontSize: 12, lineHeight: 1.7, minWidth: 0 }}>
                      <div style={{ wordBreak: 'break-word' }}>{c.meta.title ?? '（无标题）'}</div>
                      <div style={{ color: '#666' }}>
                        {c.meta.actors.length > 0 && <>演员：{c.meta.actors.join('、')}　</>}
                        {c.meta.runtime_min != null && <>{c.meta.runtime_min} 分钟　</>}
                        {c.meta.release_date && <>{c.meta.release_date}　</>}
                        {c.meta.studio && <>制作商：{c.meta.studio}　</>}
                        {c.meta.uncensored && <>无码　</>}
                      </div>
                      {c.meta.tags.length > 0 && (
                        <div style={{ color: '#888' }}>标签：{c.meta.tags.join('、')}</div>
                      )}
                      {c.meta.website && (
                        <div style={{ color: '#aaa', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                          {c.meta.website}
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}

        {manualList.length > 0 && (
          <div style={{ marginTop: 12, fontSize: 12 }}>
            <b>已保存的人工精选（{manualList.length}）</b>
            <table style={{ width: '100%', borderCollapse: 'collapse', marginTop: 6 }}>
              <tbody>
                {manualList.map((m) => (
                  <tr key={m.number}>
                    <td style={{ whiteSpace: 'nowrap', paddingRight: 8 }}>{m.number}</td>
                    <td style={{ maxWidth: 380, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {m.title ?? '-'}
                    </td>
                    <td style={{ textAlign: 'right' }}>
                      <button style={{ ...btn, padding: '1px 8px', fontSize: 11 }}
                        onClick={() => clearManual(m.number)}>取消</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {candMsg && <p style={{ fontSize: 12, color: '#c60', marginTop: 8 }}>{candMsg}</p>}
      </section>

      <section style={card}>
        <h3>扫描目录（本地）</h3>
        <div style={row}>
          <input style={input} value={dir} placeholder="视频目录绝对路径"
            onChange={(e) => setDir(e.target.value)} />
          <button style={btn} onClick={() =>
            api<{ created: number }>('/api/tasks', { method: 'POST', body: JSON.stringify({ dir }) })
              .then((j) => { setMsg(`创建 ${j.created} 条任务`); refresh(); })
              .catch((e) => setMsg(String(e)))
          }>创建任务</button>
          <button style={btn} onClick={() =>
            api<{ processed: number }>('/api/tasks/run', { method: 'POST', body: JSON.stringify({ mode: 'hard_link' }) })
              .then((j) => { setMsg(`处理 ${j.processed} 条`); refresh(); })
              .catch((e) => setMsg(String(e)))
          }>运行（硬链整理）</button>
        </div>
      </section>

      {msg && <p style={{ color: '#c60' }}>{msg}</p>}

      <section style={card}>
        <h3>任务列表</h3>
        <table style={{ width: '100%', fontSize: 13, borderCollapse: 'collapse' }}>
          <thead><tr><th>ID</th><th>番号</th><th>状态</th><th>源文件</th><th>错误</th></tr></thead>
          <tbody>
            {tasks.map((t) => (
              <tr key={t.id}>
                <td>{t.id}</td><td>{t.number ?? '-'}</td><td>{t.status}</td>
                <td style={{ maxWidth: 260, overflow: 'hidden', textOverflow: 'ellipsis' }}>{t.source_path}</td>
                <td style={{ color: '#c00' }}>{t.error ?? ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  );
}

const card: React.CSSProperties = {
  background: '#fafafa', border: '1px solid #e5e5e5', borderRadius: 8,
  padding: 16, marginBottom: 16,
};
const row: React.CSSProperties = { display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 8 };
const input: React.CSSProperties = { flex: 1, padding: '6px 10px', border: '1px solid #ccc', borderRadius: 6, width: '100%', boxSizing: 'border-box' };
const btn: React.CSSProperties = { padding: '6px 14px', cursor: 'pointer', border: '1px solid #888', borderRadius: 6, background: '#fff' };
const pre: React.CSSProperties = { background: '#f0f0f0', padding: 10, borderRadius: 6, fontSize: 12, whiteSpace: 'pre-wrap' };
const hint: React.CSSProperties = { fontSize: 12, color: '#888', margin: '4px 0 10px' };
const grid: React.CSSProperties = { display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10 };
const lbl: React.CSSProperties = { fontSize: 12, color: '#555', display: 'block' };
const ellipsis: React.CSSProperties = { maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' };
const candCard: React.CSSProperties = {
  border: '1px solid #e5e5e5', borderRadius: 8, padding: '10px 12px',
  marginBottom: 8, background: '#fff',
};

export default App;
