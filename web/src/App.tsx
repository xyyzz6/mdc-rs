import { useEffect, useMemo, useRef, useState } from 'react';
import { api, API_BASE, getToken } from './api';
import Artplayer from 'artplayer';

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
  last_stats?: string | null;
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
  dest: string;
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
  // 结果型横幅 8s 自动消失（进行中消息完成时会被新值覆盖/清除，不会误伤）
  useEffect(() => {
    if (!msg) return;
    const t = setTimeout(() => setMsg(''), 8000);
    return () => clearTimeout(t);
  }, [msg]);

  const [nd, setNd] = useState<NetdiskState | null>(null);
  const [host, setHost] = useState('');
  const [port, setPort] = useState('19798');
  const [mountRoot, setMountRoot] = useState('');
  const [prefix, setPrefix] = useState('');
  const [strmRoot, setStrmRoot] = useState('');
  const [jobsText, setJobsText] = useState('');
  const [intervalH, setIntervalH] = useState('0');
  const [status, setStatus] = useState<StrmStatus | null>(null);
  const [scan, setScan] = useState<StrmScan | null>(null);

  // 目录源：网盘是挂成本机目录（local）还是走 WebDAV（webdav —— 安卓唯一可行）
  const [srcKind, setSrcKind] = useState('webdav'); // 安卓没有 FUSE 挂载，webdav 才是对的正确默认
  const [srcBase, setSrcBase] = useState('http://127.0.0.1:19798/dav');
  const [srcUser, setSrcUser] = useState('');
  const [srcPass, setSrcPass] = useState('');
  const [srcPrefix, setSrcPrefix] = useState('');
  const [srcTimeout, setSrcTimeout] = useState('15');
  const [probe, setProbe] = useState<SourceProbe | null>(null);
  const [probeMsg, setProbeMsg] = useState('');

  // 「从网盘选择监控目录」面板：服务端经目录源列一层子目录
  const [browseOpen, setBrowseOpen] = useState(false);
  const [browseData, setBrowseData] = useState<{ kind: string; root: string; dir: string; parent: string | null; dirs: { path: string; name: string }[] } | null>(null);
  const [browseMsg, setBrowseMsg] = useState('');
  const [advOpen, setAdvOpen] = useState(false);

  const [px, setPx] = useState<ProxyStatus | null>(null);
  const [pxLog, setPxLog] = useState('');
  // 节点选择（内核 Running 时从 mihomo external-controller 拉 Selector 分组）
  const [groups, setGroups] = useState<{ name: string; now: string; all: string[] }[] | null>(null);

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
  // 筛选（本地维度：标签 / 演员 / 年份）与排序 —— 与后端搜索叠加使用
  const [libTag, setLibTag] = useState('');
  const [libActor, setLibActor] = useState('');
  const [libYear, setLibYear] = useState('');
  const [libSort, setLibSort] = useState<'new' | 'old' | 'number' | 'title'>('new');
  const [libItems, setLibItems] = useState<LibItem[] | null>(null);
  const [libTotal, setLibTotal] = useState(0);
  const [libLoading, setLibLoading] = useState(false);
  const [detail, setDetail] = useState<LibDetail | null>(null);
  const [playing, setPlaying] = useState<{ number: string; title: string; file: number } | null>(null);
  // 播放器：ArtPlayer（原生 controls 太素，参照胖5：倍速/比例/音轨/快捷键/
  // 长按快进/画中画/截图/设置面板）+ 进度记忆（localStorage）
  const pbox = useRef<HTMLDivElement | null>(null);
  const artRef = useRef<Artplayer | null>(null);
  const [rate, setRate] = useState(1);
  // 重新匹配（详情页：调多源，选中即存人工精选 + 移出库，下轮按新元数据重建）
  const [rematchOpen, setRematchOpen] = useState(false);
  const [rematchList, setRematchList] = useState<{ provider: string; status: string; error?: string; meta?: VideoMeta }[] | null>(null);
  const [rematchQuery, setRematchQuery] = useState('');
  const autoGrabbed = useRef<Set<string>>(new Set());

  // 筛选选项：从当前结果集汇总（去重 + 排序），换库自动跟着变
  const libTags = useMemo(() => {
    const s = new Set<string>();
    (libItems ?? []).forEach((e) => e.tags.forEach((t) => s.add(t)));
    return [...s].sort((a, b) => a.localeCompare(b, 'ja'));
  }, [libItems]);
  const libActors = useMemo(() => {
    const s = new Set<string>();
    (libItems ?? []).forEach((e) => e.actors.forEach((a) => s.add(a)));
    return [...s].sort((a, b) => a.localeCompare(b, 'ja'));
  }, [libItems]);
  const libYears = useMemo(() => {
    const s = new Set<string>();
    (libItems ?? []).forEach((e) => {
      const y = (e.premiered || e.year || '').slice(0, 4);
      if (y) s.add(y);
    });
    return [...s].sort().reverse();
  }, [libItems]);

  // 当前实际展示的条目 = 后端搜索结果 ∩ 三个筛选维度，再按 libSort 排序
  const shownItems = useMemo(() => {
    let out = (libItems ?? []).filter(
      (e) =>
        (!libTag || e.tags.some((t) => t === libTag)) &&
        (!libActor || e.actors.some((a) => a === libActor)) &&
        (!libYear || (e.premiered || e.year || '').slice(0, 4) === libYear),
    );
    out = [...out].sort((a, b) => {
      switch (libSort) {
        case 'old':
          return (a.premiered || a.year).localeCompare(b.premiered || b.year)
            || a.number.localeCompare(b.number);
        case 'number':
          return a.number.localeCompare(b.number);
        case 'title':
          return (a.title || a.number).localeCompare(b.title || b.number, 'ja');
        default: // new：发行日期新→旧
          return (b.premiered || b.year).localeCompare(a.premiered || a.year)
            || b.number.localeCompare(a.number);
      }
    });
    return out;
  }, [libItems, libTag, libActor, libYear, libSort]);

  const libFiltered = libTag || libActor || libYear;

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
      .then((d) => {
        setDetail(d);
        // 没海报（刮削没出图 / 无番号文件）就后台自动截一帧，一部只试一次
        if (!d.poster && !autoGrabbed.current.has(number)) {
          autoGrabbed.current.add(number);
          setTimeout(() => grabFrame(number, 0), 300);
        }
      })
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

  // 内核一跑起来就拉节点分组；运行状态下每 15s 刷新（订阅节点会变）
  const phaseKey = px ? JSON.stringify(px.phase) : '';
  useEffect(() => {
    // 内核状态有了结果（Running/Failed）就清掉「正在后台拉起内核…」横幅——
    // 不然它永远挂着，看起来像一直卡在保存中（真机截图实锤）
    if (px && typeof px.phase === 'object') {
      setMsg((m) => (m === '代理配置已保存，正在后台拉起内核…' ? '' : m));
    }
    if (px && typeof px.phase === 'object' && 'running' in px.phase) {
      loadGroups();
      const t = setInterval(loadGroups, 15000);
      return () => clearInterval(t);
    }
    setGroups(null);
  }, [phaseKey]);

  // 切回影视库视图时刷新（刚跑完一轮刮削回来看新海报）
  const switchView = (v: 'lib' | 'cfg') => {
    setView(v);
    if (v === 'lib') loadLib();
  };

  const fetchSave = () => {
    if (!nd) return Promise.reject('配置未加载');
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
      .then(() => { loadNetdisk(); loadStatus(); })
      .catch((e) => { setMsg(String(e)); throw e; });
  };

  const saveAndRun = async () => {
    if (!nd) return;
    // douyin-nas 式串联：保存完直接跑一轮（扫网盘 → 写 strm → 刮 NFO/海报），
    // 完成后媒体库立刻可见 —— 不用让用户再去找「运行一轮」
    setMsg('保存中…');
    try {
      await fetchSave();
    } catch (e) {
      setMsg(String(e));
      return;
    }
    runStrm(false);
  };

  const runStrm = (force: boolean) => {
    // 后台任务：启动即返回，轮询 status 直到 running=false（长请求在手机上会被掐断）
    setMsg(force ? '已启动强制全量重写…（进度看下方状态行）' : '已启动：正在扫描网盘并刮削…（进度看下方状态行）');
    api('/api/strm/run', { method: 'POST', body: JSON.stringify({ force }) })
      .then(() => {
        const t = setInterval(() => {
          loadStatus();
          api<StrmStatus>('/api/strm/status')
            .then((s) => {
              if (!s.running) {
                clearInterval(t);
                setMsg('');
                loadLib();
              }
            })
            .catch(() => clearInterval(t));
        }, 5000);
      })
      .catch((e) => setMsg(String(e)));
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

  // ---------- 播放进度记忆（localStorage，key = 番号/文件序号） ----------
  const progressKey = (number: string, file: number) => `${number}/${file}`;
  const loadProgress = (k: string): { pos: number; dur: number; ts: number } | undefined => {
    try {
      return (JSON.parse(localStorage.getItem('mdc_progress') || '{}') as Record<string, { pos: number; dur: number; ts: number }>)[k];
    } catch { return undefined; }
  };
  const saveProgress = (k: string, pos: number, dur: number) => {
    try {
      const all = JSON.parse(localStorage.getItem('mdc_progress') || '{}') as Record<string, { pos: number; dur: number; ts: number }>;
      all[k] = { pos, dur, ts: Date.now() };
      localStorage.setItem('mdc_progress', JSON.stringify(all));
    } catch { /* 存储满了就算了 */ }
  };
  const clearProgress = (k: string) => {
    try {
      const all = JSON.parse(localStorage.getItem('mdc_progress') || '{}');
      delete all[k];
      localStorage.setItem('mdc_progress', JSON.stringify(all));
    } catch { /* ignore */ }
  };

  // 建播放器实例（每次换片重建）。进度记忆接在 ready / timeupdate 上。
  useEffect(() => {
    if (!playing || !pbox.current) return;
    const k = progressKey(playing.number, playing.file);
    const saved = loadProgress(k);
    let art: Artplayer | null = null;
    let cancelled = false;
    art = new Artplayer({
      container: pbox.current,
      url: `${API_BASE}/api/library/${encodeURIComponent(playing.number)}/play?file=${playing.file}${mediaQuery().replace('?', '&')}`,
      poster: detail?.poster ? `${API_BASE}/api/library/${encodeURIComponent(playing.number)}/poster${mediaQuery()}` : '',
      type: 'mp4',
      autoplay: true,
      autoSize: true,
      autoMini: true,
      mutex: true,
      playsInline: true,
      theme: '#007aff',
      lang: 'zh-cn',
      // 胖5 那套能力能对上的全开：倍速 / 画面比例 / 设置面板 / 快捷键 /
      // 长按 2x 快进 / 画中画 / 截图 / 全屏（网页全屏 + 原生全屏）
      playbackRate: true,
      aspectRatio: true,
      setting: true,
      hotkey: true,
      fastForward: true,
      pip: true,
      screenshot: true,
      fullscreen: true,
      fullscreenWeb: true,
      lock: true,
      miniProgressBar: true,
      // 右键菜单只留版本号说明，长按快进由 fastForward 接管
      contextmenu: [],
    });
    artRef.current = art;
    art.on('ready', () => {
      if (!art || cancelled) return;
      art.playbackRate = rate;
      if (saved && saved.pos > 5 && saved.dur && saved.pos < saved.dur - 15) {
        art.currentTime = saved.pos;
        const mm = Math.floor(saved.pos / 60);
        const ss = String(Math.floor(saved.pos % 60)).padStart(2, '0');
        art.notice.show = `已续播到 ${mm}:${ss}`;
      }
    });
    art.on('video:timeupdate', () => {
      if (!art) return;
      const d = art.duration || 0;
      if (d > 0) saveProgress(k, art.currentTime, d);
    });
    art.on('video:ratechange', () => {
      if (art) setRate(art.playbackRate);
    });
    art.on('video:ended', () => clearProgress(k));
    return () => {
      cancelled = true;
      try { art?.destroy(false); } catch { /* ignore */ }
      artRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playing]);

  // ---------- 重新匹配（详情页：多源查询 → 采用人工精选 → 移出库待重建） ----------
  // ---------- 从视频截一帧当封面（刮削没海报时的兜底） ----------
  const grabFrame = async (number: string, file: number) => {
    setMsg('正在从视频截取封面…');
    try {
      const v = document.createElement('video');
      v.crossOrigin = 'anonymous';
      v.muted = true;
      v.playsInline = true;
      v.preload = 'auto';
      // 走同源 stream 接口（不是 302 直链）—— 跨源 video 会污染 canvas，
      // toDataURL 直接抛 SecurityError
      v.src = `${API_BASE}/api/library/${encodeURIComponent(number)}/stream?file=${file}${mediaQuery().replace('?', '&')}`;
      // 5.7GB 的 115 直链：CD2 现取向 115 要真实地址就要好几秒，40s 根本不够
      const loadWithTimeout = (ms: number) => new Promise<void>((res, rej) => {
        const t = setTimeout(() => rej(new Error('视频加载超时')), ms);
        v.onloadeddata = () => { clearTimeout(t); res(); };
        v.onerror = () => { clearTimeout(t); rej(new Error('视频加载失败')); };
      });
      try {
        await loadWithTimeout(90000);
      } catch (first) {
        // 重试一次：重新赋 src 触发全新加载（首次可能撞上直链冷启动）
        await new Promise((r) => setTimeout(r, 1500));
        v.load();
        await loadWithTimeout(90000).catch(() => { throw first; });
      }
      // 片头常是黑屏/厂牌 LOGO —— 跳到 20% 或 30 秒处再截
      const dur = v.duration || 0;
      const target = dur > 2 ? Math.min(dur * 0.2, Math.max(dur - 1, 1)) : 0;
      if (target > 0) {
        await new Promise<void>((res) => {
          const t = setTimeout(res, 25000);
          v.onseeked = () => { clearTimeout(t); res(); };
          v.currentTime = target;
        });
      }
      const c = document.createElement('canvas');
      c.width = v.videoWidth || 1280;
      c.height = v.videoHeight || 720;
      const ctx = c.getContext('2d');
      if (!ctx) throw new Error('画布不可用');
      ctx.drawImage(v, 0, 0, c.width, c.height);
      const data = c.toDataURL('image/jpeg', 0.85);
      await api(`/api/library/${encodeURIComponent(number)}/poster`, {
        method: 'POST',
        body: JSON.stringify({ file, data }),
      });
      setMsg('封面已截取');
      loadLib();
      api<LibDetail>(`/api/library/${encodeURIComponent(number)}`).then(setDetail).catch(() => {});
    } catch (e) {
      setMsg(`截帧失败：${String(e)}`);
    }
  };

  const runRematch = (number: string) => {
    if (!number.trim()) return;
    setRematchList(null);
    api<{ candidates: { provider: string; status: string; error?: string; meta?: VideoMeta }[] }>(
      '/api/scrape/candidates',
      { method: 'POST', body: JSON.stringify({ number: number.trim() }) },
    )
      .then((j) => setRematchList(j.candidates))
      .catch((e) => { setRematchList([]); setMsg(String(e)); });
  };
  const openRematch = () => {
    if (!detail) return;
    setRematchOpen(true);
    setRematchQuery(detail.number);
    runRematch(detail.number);
  };
  const adoptMeta = async (meta: VideoMeta) => {
    if (!detail) return;
    try {
      // 🔴 key 必须是**原条目的番号**：下一轮重建时按源文件解析出来的还是它，
      //    get_manual_meta(原番号) 才能命中；meta 内容用查到的正确番号。
      await api(`/api/videos/${encodeURIComponent(detail.number)}/meta`, {
        method: 'PUT',
        body: JSON.stringify({ meta }),
      });
      await api(`/api/library/${encodeURIComponent(detail.number)}`, { method: 'DELETE' });
      setMsg(`已采用「${meta.number}」——点「保存并生成 strm」按新元数据重建`);
      loadLib();
      setDetail(null);
      setRematchOpen(false);
    } catch (e) { setMsg(String(e)); }
  };

  const loadGroups = () =>
    api<{ groups: { name: string; now: string; all: string[] }[] }>('/api/proxy/groups')
      .then((j) => setGroups(j.groups))
      .catch(() => setGroups(null));

  const selectNode = (group: string, node: string) => {
    // 乐观更新 UI，切换失败时刷新回真实状态
    setGroups((cur) =>
      cur ? cur.map((g) => (g.name === group ? { ...g, now: node } : g)) : cur,
    );
    api('/api/proxy/select', {
      method: 'PUT',
      body: JSON.stringify({ group, node }),
    })
      .then(() => setMsg(`已切换到「${node}」`))
      .catch((e) => setMsg(String(e)))
      .finally(loadGroups);
  };

  const loadBrowse = (dir: string) => {
    setBrowseMsg('');
    api<{ kind: string; root: string; dir: string; parent: string | null; dirs: { path: string; name: string }[] }>(
      `/api/source/browse?dir=${encodeURIComponent(dir)}`,
    )
      .then(setBrowseData)
      .catch((e) => setBrowseMsg(String(e)));
  };
  const openBrowse = () => {
    setBrowseOpen(true);
    // 贴心起点：已填的第一个监控目录（没有才从源根开始）
    const first = jobsText.split('\n').map((s) => s.trim()).find(Boolean) ?? '';
    loadBrowse(first);
  };
  const addJob = (path: string) => {
    const cur = jobsText.split('\n').map((s) => s.trim()).filter(Boolean);
    if (cur.includes(path)) return;
    setJobsText([...cur, path].join('\n'));
  };

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
            <div className="libfilter">
              <select value={libTag} onChange={(e) => setLibTag(e.target.value)}>
                <option value="">全部标签</option>
                {libTags.map((t) => <option key={t} value={t}>{t}</option>)}
              </select>
              <select value={libActor} onChange={(e) => setLibActor(e.target.value)}>
                <option value="">全部演员</option>
                {libActors.map((a) => <option key={a} value={a}>{a}</option>)}
              </select>
              <select value={libYear} onChange={(e) => setLibYear(e.target.value)}>
                <option value="">全部年份</option>
                {libYears.map((y) => <option key={y} value={y}>{y}</option>)}
              </select>
              <select value={libSort} onChange={(e) => setLibSort(e.target.value as 'new' | 'old' | 'number' | 'title')}>
                <option value="new">最新在前</option>
                <option value="old">最早在前</option>
                <option value="number">按番号</option>
                <option value="title">按标题</option>
              </select>
              {libFiltered && (
                <button className="btn sm" type="button"
                  onClick={() => { setLibTag(''); setLibActor(''); setLibYear(''); }}>清空筛选</button>
              )}
            </div>
            <div className="countbar">
              <span className="hint" style={{ margin: 0 }}>
                共 {libTotal} 部
                {libItems && libTotal !== libItems.length ? ` · 搜索命中 ${libItems.length}` : ''}
                {shownItems.length !== (libItems?.length ?? 0) ? ` · 筛出 ${shownItems.length}` : ''}
                {libLoading ? ' · 读取中…' : ''}
              </span>
              <span className="sp" />
              <button className="tbtn" onClick={() => loadLib()}>刷新</button>
            </div>

            {libItems && libItems.length === 0 ? (
              <div className="empty">
                库还是空的 —— 去「设置」的网盘刮削页配好 CD2、跑一轮就有了。
              </div>
            ) : shownItems.length === 0 ? (
              <div className="empty">没有符合当前筛选的片子 —— 点「清空筛选」看看全部。</div>
            ) : (
              <div className="wall">
                {shownItems.map((it) => (
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
          {px && typeof px.phase === 'object' && 'running' in px.phase && (
            <>
              <div className="sub" style={{ marginTop: 14 }}>节点选择（改动立即生效；只影响走代理的出网，内网永远直连）</div>
              {!groups && <p className="hint">加载节点分组中…</p>}
              {groups && groups.length === 0 && (
                <p className="hint">订阅里没有可手动选择的分组（全是自动组）。</p>
              )}
              {groups?.map((g) => (
                <label key={g.name} className="lbl wide">
                  {g.name}（当前：{g.now}）
                  <select value={g.now} onChange={(e) => selectNode(g.name, e.target.value)}>
                    {g.all.map((n) => (
                      <option key={n} value={n}>{n}</option>
                    ))}
                  </select>
                </label>
              ))}
            </>
          )}
        </section>

        <section className="card">
          <div className="thead">目录源 {nd && <span className={`tag ${nd.source_ok ? 'ok' : 'err'}`}>{nd.source_ok ? (nd.source_kind ?? 'ok') : '配置不完整'}</span>}</div>
          <p className="hint">
            <b>三步上手（手机端默认全连内置 CD2，什么都不用改）：</b><br />
            ① 到「内置 CD2 引擎」打开管理页，登录 CD2 账号并挂载 115；<br />
            ② 下面填<b>同一组 CD2 账号密码</b>，点「保存」；<br />
            ③ 在下方「网盘刮削」里<b>从网盘选择</b>要监控的目录，点「保存并生成 strm」——
            生成 + 刮削 + 媒体库一次完成。
            桌面 / NAS 上连的是别的机器的 CD2 时，把基址改成它的地址即可。
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
                <label className="lbl">WebDAV 基址（内置 CD2 不用改）
                  <input value={srcBase} onChange={(e) => setSrcBase(e.target.value)}
                    placeholder="http://127.0.0.1:19798/dav" />
                </label>
                <label className="lbl">CD2 用户名
                  <input value={srcUser} onChange={(e) => setSrcUser(e.target.value)}
                    placeholder="和 CD2 管理页登录的同一组账号" />
                </label>
                <label className="lbl">CD2 密码
                  <input type="password" value={srcPass} onChange={(e) => setSrcPass(e.target.value)}
                    placeholder="和 CD2 管理页登录的同一组密码" />
                </label>
                {advOpen && (
                  <>
                    <label className="lbl">网盘路径前缀（一般留空）
                      <input value={srcPrefix} onChange={(e) => setSrcPrefix(e.target.value)}
                        placeholder="115open" />
                    </label>
                    <label className="lbl">超时（秒）
                      <input value={srcTimeout} onChange={(e) => setSrcTimeout(e.target.value)} />
                    </label>
                  </>
                )}
                <button className="btn sm" type="button" onClick={() => setAdvOpen((v) => !v)}>
                  {advOpen ? '收起高级选项' : '高级选项'}
                </button>
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
            <button className="btn pri" onClick={fetchSave}>保存</button>
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
            <div className="lbl wide">
              <span>要监控的网盘目录（一行一个{srcKind === 'webdav' ? '，填网盘内路径如 /115/看剧，别填网址' : ''}）</span>
              <button className="btn sm" style={{ marginLeft: 8 }} type="button" onClick={openBrowse}>从网盘选择</button>
              <textarea className="mono" value={jobsText} onChange={(e) => setJobsText(e.target.value)}
                placeholder={srcKind === 'webdav' ? '/115/看剧\n/115/新作' : '/mnt/clouddrive/115/看剧\n/mnt/clouddrive/115/新作'} />
              {browseOpen && (
                <div className="browsecard">
                  <div className="browsecrumbs">
                    <b className="ell" style={{ flex: 1 }}>{browseData?.dir ?? '加载中…'}</b>
                    {browseData?.parent && (
                      <button className="btn sm" type="button" onClick={() => loadBrowse(browseData.parent!)}>← 上级</button>
                    )}
                    <button className="btn sm" type="button" onClick={() => setBrowseOpen(false)}>收起</button>
                  </div>
                  {browseMsg && <p className="hint st-err">{browseMsg}</p>}
                  {browseData && browseData.dirs.length === 0 && (
                    <p className="hint">此目录下没有子目录 —— 直接「＋选用当前目录」即可。</p>
                  )}
                  <div className="dirslist">
                    {browseData?.dirs.map((d) => (
                      <div key={d.path} className="dirrow">
                        <button className="btn sm dirbtn ell" type="button" title={d.path}
                          onClick={() => loadBrowse(d.path)}>📁 {d.name}</button>
                        <button className="btn sm pri" type="button" title="加入监控"
                          onClick={() => addJob(d.path)}>＋</button>
                      </div>
                    ))}
                  </div>
                  {browseData && (
                    <div className="row" style={{ marginTop: 8 }}>
                      <button className="btn pri sm" type="button" onClick={() => addJob(browseData.dir)}>＋选用当前目录</button>
                    </div>
                  )}
                </div>
              )}
            </div>
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
          {status?.last_stats && !status.running && (() => {
            try {
              const s = JSON.parse(status.last_stats!) as { added: number; failed: number; skipped: number; errors: string[] };
              return (
                <p className="hint">
                  上轮结果：新增 {s.added} · 失败 {s.failed} · 跳过 {s.skipped}
                  {s.errors?.length ? <> —— 最近一条：<span className="st-err">{s.errors[0]}</span></> : null}
                </p>
              );
            } catch {
              return <p className="hint st-err">{status.last_stats}</p>;
            }
          })()}

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
            <button className="btn pri" onClick={saveAndRun}>保存并生成 strm</button>
            <button className="btn" onClick={() => runStrm(false)}>只运行一轮（用已保存配置）</button>
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
          <div className="dbox" onClick={(e) => e.stopPropagation()}>
            <button className="sx" onClick={() => setDetail(null)}>✕</button>
            <div className="dhead">
              <div className="pwrap dposter">
                {detail.poster
                  ? <img alt=""
                      src={`${API_BASE}/api/library/${encodeURIComponent(detail.number)}/poster${mediaQuery()}`} />
                  : <div className="ph">{detail.number}</div>}
              </div>
              <div className="dmain">
                <div className="dtitle">{detail.title}</div>
                <div className="dchips">
                  {(detail.premiered || detail.year) && (
                    <span className="chip">{detail.premiered?.slice(0, 4) || detail.year}</span>
                  )}
                  <span className="chip">{detail.number}</span>
                  {(detail.runtime_min ?? 0) > 0 && <span className="chip">{detail.runtime_min} 分钟</span>}
                  {detail.studio && <span className="chip">{detail.studio}</span>}
                </div>
                <button className="btn pri dplay" type="button"
                  onClick={() => play(detail.number, detail.title, 0)}>▶ 播放</button>
                {detail.tags.length > 0 && (
                  <div className="dtags">
                    {detail.tags.map((t) => <span key={t} className="chip tagc">{t}</span>)}
                  </div>
                )}
                <div className="dfile">
                  <span className="ell">{detail.files[0]?.name}</span>
                  <button className="btn sm" type="button"
                    onClick={() => navigator.clipboard?.writeText(detail.files[0]?.name ?? '')}>复制</button>
                </div>
                {detail.actors.length > 0 && (
                  <div className="dactors">演员 <b>{detail.actors.join(' / ')}</b></div>
                )}
                {(() => {
                  const p = loadProgress(progressKey(detail.number, 0));
                  if (!p || p.pos <= 5) return null;
                  const pct = p.dur ? Math.round((p.pos / p.dur) * 100) : 0;
                  if (pct >= 95) return null;
                  const mm = Math.floor(p.pos / 60), ss = String(Math.floor(p.pos % 60)).padStart(2, '0');
                  return <div className="hint st-warn">上次看到 {mm}:{ss}（{pct}%）—— 播放会自动续播</div>;
                })()}
                {detail.files.length > 1 && (
                  <div className="sfiles">
                    {detail.files.map((f, i) => (
                      <div key={i} className="sfile">
                        <span className="fn">{f.name}</span>
                        <button className="btn sm pri" type="button"
                          onClick={() => play(detail.number, detail.title, i)}>播放</button>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
            <div className="dops">
              <button className="btn danger sm" type="button"
                onClick={async () => {
                  if (!confirm(`把「${detail.title}」移出媒体库？网盘里的视频不受影响。`)) return;
                  try {
                    await api(`/api/library/${encodeURIComponent(detail.number)}`, { method: 'DELETE' });
                    setMsg(`已移出「${detail.number}」——重新匹配后点「保存并生成 strm」按新元数据重建`);
                    loadLib();
                    setDetail(null);
                  } catch (e) { setMsg(String(e)); }
                }}>移出库</button>
              <button className="btn sm" type="button"
                onClick={() => grabFrame(detail.number, 0)}>
                {detail.poster ? '重新截封面' : '截取封面'}
              </button>
              {/* 🔴 只对「长得像番号」的条目提供重新匹配：无番号文件拿文件名
                  去搜只会配出不相干的影片（真机截图实锤：7126895c_... 搜出 MUM-07） */}
              {/[A-Za-z]{2,6}-?\d{2,}/.test(detail.number) && (
                <button className="btn sm" type="button" onClick={openRematch}>重新匹配</button>
              )}
              <button className="btn sm" type="button" onClick={() => setDetail(null)}>关闭</button>
            </div>
            {rematchOpen && (
              <div className="rematch">
                <div className="dirrow">
                  <input
                    className="rinput"
                    value={rematchQuery}
                    placeholder="输入正确番号（如 ABP-123 / FC2-PPV-3141592）"
                    onChange={(e) => setRematchQuery(e.target.value)}
                    onKeyDown={(e) => { if (e.key === 'Enter') runRematch(rematchQuery); }}
                  />
                  <button className="btn sm pri" type="button"
                    onClick={() => runRematch(rematchQuery)}>查询</button>
                </div>
                <p className="hint">
                  番号猜错很常见（文件名里的编号不是真番号）—— 手动填正确番号查询，
                  命中后点「采用」，再生成一轮就按新元数据重建。
                </p>
                {!rematchList && <p className="hint">正在查询「{rematchQuery || detail.number}」…</p>}
                {rematchList && rematchList.length === 0 && <p className="hint">所有源都没查到（试试换节点或核对番号）。</p>}
                {rematchList?.map((c) => (
                  <div key={c.provider} className="dirrow">
                    <span className="ell" style={{ flex: 1 }}>
                      {c.provider} · {c.status}
                      {c.meta ? <> —— {(c.meta.title || '').slice(0, 60)}</> : c.error ? ` · ${c.error}` : ''}
                    </span>
                    {c.meta && c.status === 'hit' && (
                      <button className="btn sm pri" type="button" onClick={() => adoptMeta(c.meta!)}>采用</button>
                    )}
                  </div>
                ))}
              </div>
            )}
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
            <span className="prate">{rate}x</span>
          </div>
          <div ref={pbox} className="pstage" />
        </div>
      )}
    </>
  );
}

export default App;
