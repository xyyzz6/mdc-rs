// 前端 UI 端到端检查：用 CDP 驱动无头 Edge，真加载页面、真点按钮、收控制台错误、截图。
//
// 为什么需要它：`npm run build` 只能证明**能编译**，证明不了页面跑得起来。
// 而 SPA 最典型、最难查的故障就是「JS 一抛异常 → 整页白屏」——
// 编译期完全看不出来。这个脚本把这类问题变成一条可重复执行、有退出码的命令。
//
// 用法（Node 22+，自带全局 WebSocket，**无需任何依赖**）：
//
//   # 1. 起服务（临时数据目录 + 一个假 CD2 挂载）
//   MDC_CONFIG_PATH=<临时数据目录> MDC_BIND=127.0.0.1:19299 ./target/debug/mdc-server.exe &
//   # 2. 起无头 Edge 开调试端口（user-data-dir 指到临时目录，别碰用户 profile）
//   "$EDGE" --headless=new --disable-gpu --no-first-run \
//     --remote-debugging-port=9222 --user-data-dir=<临时目录>/edge_profile \
//     http://127.0.0.1:19299/ &
//   # 3. 检查（退出码非 0 = 有控制台错误或白屏）
//   node tools/ui_check.mjs --port 9222 --url http://127.0.0.1:19299/ --shot ui.png
//
// ⚠️ 改了前端后一定要重跑：脚本里关了浏览器缓存，否则会拿到旧 bundle，
//    表现为「改了前端但截图没变」（踩过）。
//
// ⚠️ 收尾时**按 PID 精确停进程**。不要用 `tasklist | grep msedge | taskkill` 这种宽泛过滤
//    —— `msedgewebview2.exe` 是宿主 App 自己的 WebView2 运行时，误杀会搞崩宿主。

const args = process.argv.slice(2);
const argOf = (name, dflt) => {
  const i = args.indexOf('--' + name);
  return i >= 0 && args[i + 1] ? args[i + 1] : dflt;
};

const PORT = Number(argOf('port', '9222'));
const URL_APP = argOf('url', 'http://127.0.0.1:19299/');
const SHOT = argOf('shot', 'ui.png');
/// 交互步骤：先可选取填输入框（按 placeholder 子串定位），再点按钮（按文案子串定位）。
/// 点不到不算失败，但会记进 clicked 里，便于发现「按钮没了」这类回归。
const STEPS = [
  {
    key: 'candidates',
    fill: { placeholder: '番号，如', value: 'MIDV-567' },
    click: '拉取各源结果',
    waitMs: 12000,
  },
  { key: 'useFirst', click: '用这条', waitMs: 3000 },
  { key: 'clearManual', click: '取消', waitMs: 2500 },
  { key: 'scan', click: '扫描预览', waitMs: 4000 },
  { key: 'run', click: '运行一轮', waitMs: 8000 },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function cdp(ws) {
  let id = 0;
  const pending = new Map();
  const events = [];
  ws.addEventListener('message', (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
    } else if (msg.method) {
      events.push(msg);
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const n = ++id;
      pending.set(n, { resolve, reject });
      ws.send(JSON.stringify({ id: n, method, params }));
    });
  return { send, events };
}

async function main() {
  const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  const page = list.find((t) => t.type === 'page');
  if (!page) throw new Error('没有 page target：' + JSON.stringify(list));

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', rej);
  });
  const { send, events } = cdp(ws);

  await send('Runtime.enable');
  await send('Log.enable');
  await send('Page.enable');
  await send('Network.enable');
  // 关缓存：否则 Page.navigate 到同一个 URL 会直接吃旧 index.html + 旧 bundle
  await send('Network.setCacheDisabled', { cacheDisabled: true });

  const sep = URL_APP.includes('?') ? '&' : '?';
  await send('Page.navigate', { url: `${URL_APP}${sep}t=${Date.now()}` });
  await sleep(4000);

  const evalJs = async (expr) => {
    const r = await send('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error('页面里抛异常：' + JSON.stringify(r.exceptionDetails));
    }
    return r.result.value;
  };

  const out = {};
  out.title = await evalJs('document.title');
  out.buttons = await evalJs(
    '[...document.querySelectorAll("button")].map(b => b.textContent.trim())'
  );
  // 「页面跑起来了」的最低证据：根节点有内容，而不是一个空 div
  out.rootTextLen = await evalJs(
    '(document.getElementById("root") || document.body).innerText.length'
  );
  out.bodyText = await evalJs('document.body.innerText.slice(0, 1500)');

  out.clicked = {};
  out.filled = {};
  out.afterClick = {};
  for (const s of STEPS) {
    if (s.fill) {
      // ⚠️ React 受控输入必须用**原生 value setter** + 派发 input 事件，
      // 直接 `el.value = x` 只会改 DOM，React 的 state 不变（点了等于没填）。
      out.filled[s.key] = await evalJs(`
        (() => {
          const el = [...document.querySelectorAll('input,textarea')]
            .find(x => (x.placeholder || '').includes(${JSON.stringify(s.fill.placeholder)}));
          if (!el) return false;
          const proto = el.tagName === 'TEXTAREA'
            ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
          Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(s.fill.value)});
          el.dispatchEvent(new Event('input', { bubbles: true }));
          return true;
        })()
      `);
    }
    out.clicked[s.key] = await evalJs(`
      (() => {
        const b = [...document.querySelectorAll("button")]
          .find(x => x.textContent.includes(${JSON.stringify(s.click)}));
        if (!b) return false;
        b.click();
        return true;
      })()
    `);
    await sleep(s.waitMs);
    out.afterClick[s.key] = await evalJs('document.body.innerText.slice(-1200)');
  }

  // 控制台/异常必须为空 —— 这是「没白屏」的硬证据
  out.consoleErrors = events
    .filter((e) => e.method === 'Runtime.consoleAPICalled' && e.params.type === 'error')
    .map((e) => e.params.args.map((a) => a.value ?? a.description ?? '').join(' '));
  out.exceptions = events
    .filter((e) => e.method === 'Runtime.exceptionThrown')
    .map((e) => e.params.exceptionDetails.text);
  out.logErrors = events
    .filter((e) => e.method === 'Log.entryAdded' && e.params.entry.level === 'error')
    .map((e) => e.params.entry.text);

  const shot = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
  const fs = await import('node:fs');
  fs.writeFileSync(SHOT, Buffer.from(shot.data, 'base64'));
  out.screenshot = SHOT;

  console.log(JSON.stringify(out, null, 2));

  const bad = out.consoleErrors.length + out.exceptions.length + out.logErrors.length;
  if (bad > 0 || out.rootTextLen === 0) {
    console.error(`\n❌ UI 检查失败：控制台错误/异常 ${bad} 条，根节点文本长度 ${out.rootTextLen}`);
    process.exitCode = 1;
  } else {
    console.error(`\n✅ UI 检查通过：无控制台错误，根节点渲染了 ${out.rootTextLen} 字`);
  }
  ws.close();
}

main().catch((e) => {
  console.error('FAILED:', e.message);
  process.exit(1);
});
