// SPDX-License-Identifier: MIT
// Static UI. Only fixed appearance enums persist; account/session data never do.
'use strict';

const $ = (selector) => document.querySelector(selector);
const tauri = window.__TAURI__;
const bridge = Boolean(tauri?.core?.invoke && tauri?.event?.listen);
// The public source uses text marks. Optional provider artwork is kept local
// until its redistribution terms are established separately.
const brandAssets = Object.freeze({});
const platforms = [
  { id: 'bilibili', name: 'Bilibili', kind: 'social', label: '视频与社区', tone: 'rose', description: '视频、动态与评论' },
  { id: 'qq', name: 'QQ', kind: 'social', label: '聊天', tone: 'blue', description: '消息、群聊与附件' },
  { id: 'tieba', name: '贴吧', kind: 'social', label: '社区', tone: 'indigo', description: '帖子与楼中楼讨论' },
  { id: 'x', name: 'X', kind: 'social', label: '网页会话', tone: 'carbon', description: '浏览器登录，自动连接账号' },
  { id: 'pixiv', name: 'pixiv', kind: 'social', label: '插画与漫画', tone: 'blue', description: '作品、推荐与按需读图' },
  { id: 'github', name: 'GitHub', kind: 'application', label: '开发协作', tone: 'stone', description: '仓库、Issue 与 PR' },
  { id: 'codex', name: 'Codex', kind: 'agent', label: '订阅登录', tone: 'sage', description: '原生 Codex 订阅 · 共享人格与记忆' },
  { id: 'antigravity', name: 'Antigravity', kind: 'agent', label: '订阅登录', tone: 'iris', description: 'Google 订阅授权 · 共享人格与记忆' },
  { id: 'weixin', name: '微信', kind: 'social', label: '已暂停', tone: 'mint', description: '个人账号测试暂停，不发起登录', paused: true },
];
const states = new Map(platforms.map((platform) => [platform.id, {
  status: platform.paused ? 'paused' : 'unknown',
  detail: platform.description,
  capabilities: null,
  checked: false,
  error: null,
}]));
let connection = { mode: 'local', destination: null, runtime_binary: null, state: '.runtime/state', account: 'main' };
let selectedMode = 'local';
let active = null;
let refreshing = false;
let connectionDirty = false;
let generation = 0;
let requiredInput = null;
let cookieInputMode = 'fields';
let xWindowOpening = null;
let xBrowserOpening = null;
let toastTimer = null;
const activity = [];
const memorySamples = [];
let memoryBasis = null;
let memoryTimer = null;
let memoryInFlight = false;
let memoryPaused = false;
let memoryDisposed = false;
let memoryGeneration = 0;
const appearanceOptions = Object.freeze({
  theme: Object.freeze({ key: 'agent.operator.appearance.theme', values: Object.freeze(['iris', 'lagoon', 'sunset']), fallback: 'iris' }),
  density: Object.freeze({ key: 'agent.operator.appearance.density', values: Object.freeze(['comfortable', 'compact']), fallback: 'comfortable' }),
});

function setAppearance(kind, value, persist = true) {
  if (!Object.prototype.hasOwnProperty.call(appearanceOptions, kind)) return false;
  const option = appearanceOptions[kind];
  if (!option.values.includes(value)) return false;
  document.documentElement.dataset[kind] = value;
  document.querySelectorAll(`[data-${kind}-choice]`).forEach((button) => {
    button.setAttribute('aria-pressed', String(button.dataset[`${kind}Choice`] === value));
  });
  if (persist) {
    try { window.localStorage.setItem(option.key, value); }
    catch (_) { /* Appearance remains usable when local storage is unavailable. */ }
  }
  return true;
}
function restoreAppearance() {
  for (const [kind, option] of Object.entries(appearanceOptions)) {
    let value = option.fallback;
    try {
      const stored = window.localStorage.getItem(option.key);
      if (option.values.includes(stored)) value = stored;
      else if (stored !== null) window.localStorage.removeItem(option.key);
    } catch (_) { /* Storage is optional; login never depends on it. */ }
    setAppearance(kind, value, false);
  }
}
function memoryMiB(bytes) { return (bytes / 1048576).toLocaleString(window.GlimlinkI18n?.language() || 'en', { maximumFractionDigits: 1 }); }
function validBytes(value) { return Number.isSafeInteger(value) && value >= 0; }
function memoryInterval() { return connection.mode === 'ssh' ? 5000 : 3000; }
function clearMemory(message) {
  for (const id of ['memory-value','memory-peak','memory-limit','memory-time']) $('#'+id).textContent = '—';
  $('#memory-metric').textContent = '无可用数据';
  $('#memory-message').textContent = message;
  $('#memory-time').removeAttribute('datetime');
  $('#memory-trend').setAttribute('points', '');
  memorySamples.length = 0; memoryBasis = null;
}
function renderMemory(sample) {
  const container = sample?.scope === 'runtime_container';
  const local = sample?.scope === 'local_runtime_process_tree';
  $('#memory-source').textContent = container ? 'Runtime · VPS 容器' : 'Runtime · 本机';
  $('#memory-scope').textContent = container ? '包含 Runtime 容器与原生引擎，不含操控端或其他服务。' : '包含 Runtime 及其原生引擎子进程，不含操控端。';
  const stopped = sample?.running === false;
  const pss = local && validBytes(sample.pss_bytes);
  const value = stopped ? 0 : container ? sample.current_bytes : pss ? sample.pss_bytes : sample?.rss_bytes;
  if (!sample?.available || (!container && !local) || !validBytes(value)) {
    clearMemory('当前连接的 Runtime 内存暂不可用。'); $('#memory-state').textContent = '暂不可用'; return;
  }
  const basis = [connection.mode,connection.destination,connection.state,container?'cgroup':pss?'pss':'rss',stopped].join('/');
  if (memoryBasis !== basis) { memorySamples.length = 0; memoryBasis = basis; }
  memorySamples.push(value); if (memorySamples.length > 24) memorySamples.shift();
  $('#memory-metric').textContent = stopped ? '后台未运行' : container ? '容器内存 · 含缓存' : pss ? 'PSS · 分摊共享内存' : 'RSS · 驻留内存';
  $('#memory-value').textContent = memoryMiB(value);
  $('#memory-peak').textContent = validBytes(sample.peak_bytes) ? `${memoryMiB(sample.peak_bytes)} MiB` : '—';
  $('#memory-limit').textContent = validBytes(sample.limit_bytes) && sample.limit_bytes>0 ? `${memoryMiB(sample.limit_bytes)} MiB` : '—';
  $('#memory-message').textContent = stopped ? 'Runtime 后台已停止。' : container ? '直接读取容器内存，峰值按本次容器运行累计。' : sample.partial ? '部分进程未能读取，统计可能不完整。RSS 会重复计入共享页。' : 'RSS 会重复计入共享页；PSS 按比例分摊。';
  $('#memory-state').textContent = memoryPaused ? '已暂停' : document.hidden ? '后台暂停' : connection.mode==='ssh' ? '每 5 秒采样' : '每 3 秒采样';
  const when = new Date(sample.sampled_at_unix_ms);
  $('#memory-time').textContent = when.toLocaleTimeString(window.GlimlinkI18n?.language() || 'en', { hour12: false });
  $('#memory-time').dateTime = when.toISOString();
  const ceiling = Math.max(1048576,...memorySamples) * 1.15;
  const points = memorySamples.map((bytes,index) => `${memorySamples.length===1?0:index/(memorySamples.length-1)*100},${37-bytes/ceiling*32}`);
  if(points.length===1)points.push(`100,${37-value/ceiling*32}`);
  $('#memory-trend').setAttribute('points',points.join(' '));
}
function scheduleMemory() {
  clearTimeout(memoryTimer); memoryTimer = null;
  if(bridge && !connectionDirty && !memoryPaused && !memoryDisposed && !document.hidden && window.GlimlinkPage==='settings') memoryTimer=setTimeout(sampleMemory,memoryInterval());
}
async function sampleMemory() {
  if(!bridge || connectionDirty || active || memoryInFlight || memoryPaused || memoryDisposed || document.hidden || window.GlimlinkPage!=='settings') {scheduleMemory();return;}
  memoryInFlight=true; const generation=memoryGeneration; const target={...connection};
  try {
    const sample=await invoke('memory_snapshot',{connection:target});
    if(!memoryDisposed && generation===memoryGeneration && !connectionDirty)renderMemory(sample);
  } catch(_) {
    if(!memoryDisposed && generation===memoryGeneration) {clearMemory('内存采样未完成，稍后自动重试。');$('#memory-state').textContent=memoryPaused?'已暂停':'采样失败';}
  } finally {memoryInFlight=false;scheduleMemory();}
}
function toggleMemory() {
  memoryPaused = !memoryPaused;
  $('#memory-toggle').textContent = memoryPaused ? '继续' : '暂停';
  $('#memory-toggle').setAttribute('aria-pressed', String(memoryPaused));
  $('#memory-toggle').setAttribute('aria-label', memoryPaused ? '继续内存采样' : '暂停内存采样');
  $('#memory-state').textContent = memoryPaused ? '已暂停' : '等待采样';
  clearTimeout(memoryTimer); memoryTimer = null;
  if (!memoryPaused) sampleMemory();
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function platformFor(id) { return platforms.find((platform) => platform.id === id); }
function publicCode(error) {
  const code = error?.code || error?.error?.code;
  return typeof code === 'string' && /^[a-zA-Z0-9_.:-]{1,80}$/.test(code) ? code : 'operation_failed';
}
function publicMessage(error) {
  if (publicCode(error) === 'state_busy') return '后台正在使用账号状态，请先在调控页停止后台，再登录。';
  const message = error?.message || error?.error?.message;
  if (typeof message === 'string') return message.slice(0, 500);
  if (typeof error === 'string') return error.slice(0, 500);
  return '操作未完成，请检查连接配置或查看错误码。';
}
function unpack(value) {
  if (value && value.type === 'response') {
    if (value.ok === false) throw value.result;
    return value.result;
  }
  return value;
}
async function invoke(command, arguments_) {
  if (!bridge) throw { code: 'preview_only', message: '预览模式不执行真实账号操作。' };
  return unpack(await tauri.core.invoke(command, arguments_));
}
function note(message) {
  activity.unshift({ time: new Date(), message });
  if (activity.length > 12) activity.pop();
  const list = $('#activity-list');
  list.replaceChildren(...activity.map((item) => {
    const row = element('li');
    row.append(element('time', '', item.time.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false })), element('i', 'activity-dot'), element('span', '', item.message));
    return row;
  }));
}
function toast(message) {
  const node = $('#toast');
  node.textContent = message;
  node.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { node.hidden = true; node.textContent = ''; }, 4500);
}
function presentation(status) {
  const map = {
    unknown: ['未检查', ''], checking: ['检查中', 'pending'], starting: ['启动中', 'pending'],
    saved_session: ['会话已保存', 'saved'], saved_token: ['Token 已保存', 'saved'],
    online: ['已登录', 'verified'], authenticated: ['已登录', 'verified'],
    signed_out: ['未登录', ''], needs_auth: ['需要登录', ''], login_required: ['需要登录', ''],
    pending: ['等待确认', 'pending'], verification_required: ['等待验证', 'pending'],
    verification_expired: ['验证已过期', 'failed'], failed: ['未完成', 'failed'],
    cancelled: ['已取消', ''], canceled: ['已取消', ''], paused: ['已暂停', ''],
    testing_paused: ['已暂停', ''], rate_limited: ['暂时受限', 'failed'],
  };
  return map[status] || ['状态待确认', ''];
}
function safeIdentity(value) {
  const identity = value?.identity;
  const user = value?.user;
  if (typeof value?.display_name === 'string' && value.display_name) return value.display_name.slice(0,180);
  if (user && typeof user === 'object') {
    const name = user.display_name || user.name || user.uname || user.nickname || user.user_name || user.login || user.screen_name;
    if (typeof name === 'string' && name) return name.slice(0,180);
  }
  if (identity && typeof identity === 'object') return [identity.label || identity.name || identity.display_name, identity.id].filter((item) => typeof item === 'string' || typeof item === 'number').join(' · ').slice(0, 180);
  if (typeof identity === 'string' || typeof identity === 'number') return String(identity).slice(0, 180);
  if (user) return [user.name || user.login || user.display_name, user.id || user.mid || user.user_id].filter((item) => typeof item === 'string' || typeof item === 'number').join(' · ').slice(0, 180);
  return '';
}
function render() {
  const cards = platforms.map((platform) => {
    const state = states.get(platform.id);
    const [statusText, statusClass] = presentation(state.status);
    const current = active?.platform === platform.id;
    const paused = platform.paused || state.capabilities?.paused;
    const card = element('article', `account-card tone-${platform.tone}${paused ? ' paused' : ''}${current ? ' active' : ''}${statusClass === 'verified' ? ' verified' : ''}`);
    card.dataset.platform = platform.id;
    card.dataset.kind = platform.kind;
    card.setAttribute('aria-label', `${platform.name}，${statusText}`);
    const top = element('div', 'account-top');
    const icon = element('div', 'platform-icon');
    if (brandAssets[platform.id]) {
      const logo = element('img', 'platform-logo');
      logo.src = brandAssets[platform.id];
      logo.alt = `${platform.name} 标志`;
      logo.width = 28; logo.height = 28;
      logo.addEventListener('error', () => { const initial=element('span','brand-fallback',(window.GlimlinkI18n?.text(platform.name)||platform.name).slice(0,1));initial.setAttribute('translate','no');icon.replaceChildren(initial); });
      icon.append(logo);
    } else {
      const initial=element('span','brand-fallback',(window.GlimlinkI18n?.text(platform.name)||platform.name).slice(0,1));initial.setAttribute('translate','no');icon.append(initial);
    }
    const badge = element('span', `account-state ${statusClass}`, statusText);
    top.append(icon, badge);
    const name = element('div', 'account-name');
    name.append(element('h3', '', platform.name), element('span', 'platform-tag', platform.label));
    const accountName=element('p',`account-display-name${state.displayName?'':' pending-identity'}`,state.displayName || (['saved_session','saved_token'].includes(state.status)?'已保存账号，平台暂未提供名称':['online','authenticated'].includes(state.status)?'已登录，平台暂未提供名称':state.checked?'未登录':'未核验账号'));
    if(state.displayName)accountName.setAttribute('translate','no');
    const detail = element('p', 'account-detail', state.detail);
    detail.title = state.detail;
    const actions = element('div', 'account-actions');
    const login = element('button', 'login-button', paused ? '暂停测试' : current ? (active.id ? '取消登录' : '正在启动…') : state.status === 'authenticated' || state.status === 'online' ? '核验与复用 ↗' : platform.id === 'github' ? '在浏览器中授权 ↗' : platform.id === 'x' ? '连接 X ↗' : '在浏览器中登录 ↗');
    login.disabled = !bridge || paused || refreshing || connectionDirty || (active && !current) || (current && !active.id);
    login.addEventListener('click', () => current ? cancelLogin() : beginLogin(platform.id, platform.id === 'qq' ? 'qr' : 'auto'));
    const check = element('button', 'account-check');
    check.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18 8a7 7 0 1 0 .4 7M18 3v5h-5"/></svg>';
    check.title = `检查 ${platform.name} 状态`;
    check.setAttribute('aria-label', check.title);
    check.disabled = !bridge || paused || Boolean(active) || refreshing || connectionDirty || state.status === 'checking';
    check.addEventListener('click', () => checkOne(platform.id));
    actions.append(login, check);
    card.append(top, name, accountName, detail, actions);
    if (state.error === 'state_busy' && !paused) {
      const stop = element('button','login-worker-stop','停止后台后登录');stop.type='button';stop.disabled=!bridge || Boolean(active) || refreshing || connectionDirty;
      stop.addEventListener('click',()=>stopWorkerAndLogin(platform.id));card.append(stop);
    }
    if (platform.id === 'github') {
      const manual=element('button','x-refresh-login','使用已有 Token');manual.type='button';manual.disabled=!bridge || Boolean(active) || refreshing || connectionDirty;manual.addEventListener('click',()=>beginLogin('github','token'));card.append(manual);
    }
    if (['bilibili', 'tieba'].includes(platform.id)) {
      const qr = element('button', 'x-refresh-login', '改用扫码登录');
      qr.type='button';qr.disabled=!bridge || Boolean(active) || refreshing || connectionDirty;
      qr.addEventListener('click',()=>beginLogin(platform.id,'qr'));card.append(qr);
    }
    if (['x', 'bilibili', 'tieba', 'pixiv'].includes(platform.id)) {
      const update = element('button', 'x-refresh-login', '重新登录');
      update.type = 'button';
      update.disabled = !bridge || paused || refreshing || connectionDirty || Boolean(active);
      update.addEventListener('click', () => beginLogin(platform.id, 'browser'));
      card.append(update);
    }
    return card;
  });
  for (const [kind, selector] of [['agent', '#agent-grid'], ['social', '#account-grid'], ['application', '#application-grid']]) {
    $(selector).replaceChildren(...cards.filter((card) => card.dataset.kind === kind));
  }
  const values = [...states.values()];
  const checked = values.some((state) => state.checked);
  $('#verified-count').textContent = checked ? String(values.filter((state) => ['online', 'authenticated'].includes(state.status)).length) : '—';
  $('#saved-count').textContent = checked ? String(values.filter((state) => ['saved_session', 'saved_token'].includes(state.status)).length) : '—';
  $('#active-status').textContent = active ? `${platformFor(active.platform)?.name} · 登录中` : refreshing ? '正在逐个检查' : '等待你的操作';
  $('#active-description').textContent = active ? '同一会话持续验证' : refreshing ? '不会自动发起新登录' : '优先复用已有登录态';
  $('#active-light').classList.toggle('busy', Boolean(active) || refreshing);
  $('#cancel-active').hidden = !active;
  $('#cancel-active').disabled = !active?.id;
  $('#refresh-all').disabled = !bridge || Boolean(active) || refreshing || connectionDirty;
  $('#connection-form').querySelectorAll('input,button,summary').forEach((node) => { if ('disabled' in node) node.disabled = Boolean(active) || refreshing || (!bridge && node.type === 'submit'); });
}
function markBridgeAvailable() {
  const badge = $('#bridge-badge');
  badge.className = 'bridge-badge available';
  badge.replaceChildren(element('i'), document.createTextNode('控制通道可用'));
}
async function checkOne(platform, quiet = false) {
  if (active || !bridge || connectionDirty) return;
  const state = states.get(platform);
  state.status = 'checking'; state.error = null; state.detail = '读取可用方式与已有登录状态…'; render();
  try {
    const capabilities = await invoke('platform_capabilities', { connection, platform });
    state.capabilities = capabilities;
    if (capabilities.paused || platformFor(platform).paused) {
      state.status = 'paused'; state.detail = '个人账号测试暂停，不执行任何账号请求。';
    } else {
      const value = await invoke('account_status', { connection, platform });
      state.displayName=safeIdentity(value);
      state.status = value.status || (value.authenticated === true ? 'authenticated' : 'unknown');
      state.detail = state.displayName ? '账号名称来自已连接的平台。' : (state.status === 'saved_session' || state.status === 'saved_token' ? '本地已保存，尚未核验服务端有效性' : state.status === 'signed_out' ? `账号别名 ${connection.account} 尚未登录` : platformFor(platform).description);
    }
    state.checked = true;
    markBridgeAvailable();
    if (!quiet) note(`${platformFor(platform).name} · ${presentation(state.status)[0]}`);
  } catch (error) {
    state.status = 'failed'; state.error = publicCode(error); state.detail = `${state.error} · ${publicMessage(error)}`;
    if (!quiet) note(`${platformFor(platform).name} · 检查未完成 (${state.error})`);
  }
  render();
}
async function refreshAll() {
  if (!bridge || active || refreshing || connectionDirty) return;
  refreshing = true; render();
  try { for (const platform of platforms) await checkOne(platform.id, true); }
  finally { refreshing = false; render(); }
  note('已检查账号状态，没有自动发起登录');
}
async function stopWorkerAndLogin(platform) {
  if (!bridge || active || connectionDirty) return;
  try {
    const snapshot=await invoke('control_request',{connection,command:{action:'snapshot'}});
    if(snapshot.host?.model_process_active || snapshot.jobs?.some(j=>j.status==='running'))throw {code:'worker_active',message:'后台正在执行一轮，请等待结束，或在调控页主动停止。'};
    await invoke('worker_control',{connection,start:false});
    note('后台已停止，登录后可在调控页重新启动。');
    await beginLogin(platform,platform==='qq'?'qr':'auto');
  } catch(error) {const state=states.get(platform);state.error=publicCode(error);state.detail=publicMessage(error);render();}
}
async function beginLogin(platform, method = 'auto') {
  if (!bridge || active || connectionDirty || platformFor(platform).paused) return;
  const state = states.get(platform);
  const run = ++generation;
  active = { id: null, platform, generation: run, events: [] };
  state.status = 'starting'; state.detail = '先核验已有状态，再选择可用登录方式。'; state.error = null;
  render();
  note(`${platformFor(platform).name} · 开始登录检查`);
  try {
    const result = await invoke('begin_login', { connection, platform, method, secret: null });
    if (active?.generation === run) {
      if (typeof result?.login_id !== 'string' || !result.login_id || result.login_id.length > 128) throw { code: 'invalid_login_response', message: '核心没有返回有效的登录会话标识。' };
      active.id = result.login_id;
      state.status = 'pending'; state.detail = '登录会话已建立，等待验证状态。';
      const queued = active.events; active.events = [];
      for (const payload of queued) if (payload.login_id === result.login_id) handleLoginEvent(payload);
      render();
    }
  } catch (error) {
    if (active?.generation !== run) return;
    state.status = 'failed'; state.error = publicCode(error); state.detail = `${state.error} · ${publicMessage(error)}`;
    active = null; clearInput(); render();
    note(`${platformFor(platform).name} · 未完成 (${state.error})`);
  }
}
async function cancelLogin() {
  if (!active?.id) return;
  const id = active.id;
  clearInput();
  try {
    await invoke('cancel_login', { loginId: id });
    // The core owns the final outcome. It can report authenticated when a
    // completion was already committed before cancellation reached it.
    if (active?.id === id) {
      states.get(active.platform).detail = '正在关闭验证会话，等待最终结果…';
      clearInput(); render();
    }
  } catch (error) {
    toast(`取消请求未完成：${publicCode(error)}`);
  }
}
function clearPrivateFields() {
  for (const selector of ['#input-value', '#input-uin', '#input-auth-token', '#input-ct0', '#input-cookie-header']) $(selector).value = '';
}
function clearInput() {
  clearPrivateFields();
  $('#x-import-details').open = false;
  $('#x-native-details').open = false;
  $('#input-error').textContent = '';
  $('#x-window-feedback').textContent = '';
  $('#x-browser-feedback').textContent = '';
  if ($('#input-dialog').open) $('#input-dialog').close();
  requiredInput = null;
}
function syncCookieImport() {
  const cookies = requiredInput?.kind === 'cookies';
  const importing = cookies && $('#x-import-details').open;
  for (const [selector, selected] of [['#input-cookie-header', cookieInputMode === 'header'], ['#input-auth-token', cookieInputMode === 'fields'], ['#input-ct0', cookieInputMode === 'fields']]) {
    $(selector).disabled = !importing || !selected;
    $(selector).required = importing && selected;
  }
  $('#input-submit').hidden = cookies && !importing;
}
function setCookieInputMode(mode) {
  if (!['header', 'fields'].includes(mode)) return;
  cookieInputMode = mode;
  clearPrivateFields();
  $('#cookie-header-field').hidden = mode !== 'header';
  $('#cookie-pair-fields').hidden = mode !== 'fields';
  $('#cookie-header-guide').hidden = mode !== 'header';
  $('#cookie-pair-guide').hidden = mode !== 'fields';
  syncCookieImport();
  document.querySelectorAll('[data-cookie-mode]').forEach((button) => button.setAttribute('aria-pressed', String(button.dataset.cookieMode === mode)));
}
async function openXWindow() {
  if (requiredInput?.kind !== 'cookies' || active?.platform !== 'x' || active.id !== requiredInput.loginId || xWindowOpening) return;
  const loginId = active.id;
  clearPrivateFields();
  $('#x-import-details').open = false;
  syncCookieImport();
  xWindowOpening = loginId;
  $('#x-window-open').disabled = true;
  $('#x-window-feedback').textContent = '正在打开专用登录窗口…';
  try {
    await invoke('open_x_login_window', { loginId });
    if (requiredInput?.loginId === loginId) $('#x-window-feedback').textContent = '请在 X 窗口正常登录，无需复制信息。完成后等待这里显示核验结果。';
  } catch (error) {
    if (requiredInput?.loginId === loginId) $('#x-window-feedback').textContent = `窗口未打开 (${publicCode(error)})，可以继续私密导入。`;
  } finally {
    if (xWindowOpening === loginId) { xWindowOpening = null; $('#x-window-open').disabled = false; }
  }
}
async function openXBrowser() {
  if (requiredInput?.kind !== 'cookies' || active?.platform !== 'x' || active.id !== requiredInput.loginId || xBrowserOpening) return;
  const loginId = active.id;
  clearPrivateFields();
  xBrowserOpening = loginId;
  $('#x-browser-open').disabled = true;
  try {
    await invoke('open_x_in_browser', { loginId });
    if (requiredInput?.loginId === loginId) {$('#x-import-details').open=true;syncCookieImport();$('#x-browser-feedback').textContent = '浏览器已打开。登录后按下方步骤复制两项信息，再点“验证并连接”；不会自动回传。';}
  } catch (error) {
    if (requiredInput?.loginId === loginId) $('#x-browser-feedback').textContent = `浏览器未打开 (${publicCode(error)})，可以继续私密导入。`;
  } finally {
    if (xBrowserOpening === loginId) { xBrowserOpening = null; $('#x-browser-open').disabled = false; }
  }
}
function showInput(data, loginId) {
  if (!active || active.id !== loginId) return;
  const kind = data.kind;
  const names = { password: ['输入 QQ 登录信息', '密码'], authorization_code: ['完成订阅授权', '一次性授权码'], token: ['连接 GitHub 账号', '访问 Token'], session: ['导入已有登录态', '本机私密文件路径'], cookies: ['连接 X 账号', '浏览器登录信息'] };
  if (!names[kind]) { toast('这个输入类型尚未支持，请取消本次登录。'); return; }
  clearInput(); requiredInput = { kind, loginId };
  $('#input-platform').textContent = `${platformFor(active.platform).name} · 私密输入`;
  $('#input-title').textContent = names[kind][0];
  $('#input-label').textContent = names[kind][1];
  $('#input-message').textContent = kind === 'cookies' ? '用浏览器完成登录，再把这个账号连接到 Runtime。' : data.message || '通过当前私有控制通道提交，不进入 AI 上下文。';
  $('#uin-field').hidden = kind !== 'password';
  $('#input-value-field').hidden = kind === 'cookies';
  $('#input-value').required = kind !== 'cookies';
  $('#input-value').disabled = kind === 'cookies';
  $('#cookie-fields').hidden = kind !== 'cookies';
  setCookieInputMode('fields');
  $('#input-submit-label').textContent = kind === 'cookies' ? '验证并连接' : '安全提交';
  $('#input-value').type = kind === 'session' ? 'text' : 'password';
  $('#input-value').placeholder = kind === 'session' ? '/private/account-session.json' : '';
  $('#input-privacy').textContent = kind === 'session' ? '文件只从操控端读取。请使用权限 0600 的私有普通文件。' : kind === 'cookies' ? '仅通过私有控制通道交给 Runtime，不进入 AI 对话、活动记录或本页持久存储。提交后立即清空。' : '只用于当前登录。提交后立即清空本页输入。';
  $('#input-dialog').showModal();
  (kind === 'password' ? $('#input-uin') : kind === 'cookies' ? $('#x-browser-open') : $('#input-value')).focus();
}
async function submitInput(event) {
  event.preventDefault();
  if (!requiredInput || !active || active.id !== requiredInput.loginId) return;
  const { kind, loginId } = requiredInput;
  const input = $('#input-value');
  let value = input.value;
  if (kind === 'cookies') {
    if (!$('#x-import-details').open) return;
    if (cookieInputMode === 'header') {
      value = $('#input-cookie-header').value.trim();
      if (!value) { $('#input-error').textContent = '请粘贴完整 Cookie 请求头，或改为分别填写。'; return; }
    } else {
      const auth_token = $('#input-auth-token').value.trim();
      const ct0 = $('#input-ct0').value.trim();
      if (!auth_token || !ct0) { $('#input-error').textContent = '请填写 auth_token 与 ct0 两项 Cookie。'; return; }
      value = JSON.stringify({ auth_token, ct0 });
    }
  } else if (kind === 'password') {
    const uin = Number($('#input-uin').value.trim());
    if (!Number.isSafeInteger(uin) || uin < 1 || uin > 4294967295) { $('#input-error').textContent = '请输入有效的 QQ 账号。'; return; }
    value = JSON.stringify({ uin, password: value });
  }
  if (!value.trim()) return;
  clearPrivateFields();
  $('#input-submit').disabled = true;
  try {
    await invoke('submit_login_input', { loginId, kind, value });
    value = '';
    if (requiredInput?.loginId === loginId) clearInput();
    if (active?.id === loginId) { states.get(active.platform).detail = '验证信息已提交，等待核心确认…'; render(); }
  } catch (error) {
    value = '';
    $('#input-error').textContent = `提交未完成 (${publicCode(error)})，输入已清空。`;
  } finally { $('#input-submit').disabled = false; }
}
function handleLoginEvent(payload) {
  if (!payload || typeof payload.login_id !== 'string' || typeof payload.platform !== 'string') return;
  if (!active || active.platform !== payload.platform || (active.id && active.id !== payload.login_id)) return;
  if (!active.id) {
    if (active.events.length < 32) active.events.push(payload);
    return;
  }
  const state = states.get(payload.platform);
  const data = payload.data || {};
  if (payload.kind === 'finished') {
    const status = data.status || 'failed';
    state.status = status; state.checked = true;
    const displayName=safeIdentity(data);if(displayName)state.displayName=displayName;
    state.error = data.error?.code ? publicCode(data.error) : null;
    state.detail = status === 'authenticated' ? (data.reused_session ? '已核验并复用原有登录态' : '登录已确认，账号状态已保存') : data.error ? `${publicCode(data.error)} · ${publicMessage(data.error)}` : status === 'cancelled' ? '验证会话已关闭，已有登录态保留。' : '操作已结束，请检查账号状态。';
    note(`${platformFor(payload.platform).name} · ${presentation(status)[0]}${state.error ? ` (${state.error})` : ''}`);
    active = null; clearInput(); render();
    if (status === 'authenticated') markBridgeAvailable();
    return;
  }
  if (payload.kind === 'input_required') {
    state.status = 'verification_required'; state.detail = '等待你在操控端提供验证信息。';
    showInput(data, payload.login_id);
  } else if (payload.kind === 'authorization') {
    state.status = 'verification_required';
    state.detail = '请在系统浏览器完成官方授权，返回这里继续。';
    // Authorization URLs and query parameters never enter UI activity or DOM.
    if (typeof data.user_code === 'string' && data.user_code.length <= 256 && !/[\u0000-\u001f\u007f]/.test(data.user_code)) {
      state.detail = `在官方授权页输入临时代码：${data.user_code}`;
    }
  } else if (payload.kind === 'progress') {
    if (data.private_input_submitted === true) clearInput();
    if (data.browser_handoff === true && requiredInput?.kind === 'cookies' && active.platform === 'x') {
      $('#x-native-details').open=false;$('#x-import-details').open=true;syncCookieImport();
      $('#x-browser-feedback').textContent='浏览器已打开。登录后按下方步骤复制两项信息，再点“验证并连接”；不会自动回传。';
    }
    state.status = data.status || 'pending';
    state.detail = data.challenge ? '请在登录验证窗口中手动完成验证。' : data.qr ? '请在登录窗口扫码，并在手机上确认。' : data.message || '等待平台确认，核心保持当前认证会话。';
  }
  render();
}
function markConnectionDirty() {
  if (active || refreshing) return;
  connectionDirty = true;
  $('#connection-feedback').textContent = bridge ? '配置已修改，请应用后再操作账号。' : '预览配置可编辑，本页不发起真实连接。';
  $('#connection-feedback').classList.remove('error');
  render();
}
function setMode(mode) {
  if (active || refreshing) return;
  selectedMode = mode;
  document.querySelectorAll('.segment').forEach((button) => { const selected = button.dataset.mode === mode; button.classList.toggle('selected', selected); button.setAttribute('aria-pressed', String(selected)); });
  $('.ssh-field').hidden = mode !== 'ssh';
  $('#runtime-binary').placeholder = mode === 'ssh' ? '远端 agent-runtime 路径，可选' : '默认使用相邻 agent-runtime';
  markConnectionDirty();
}
async function applyConnection(event) {
  event.preventDefault();
  if (active || refreshing || !bridge) return;
  const state = $('#state').value.trim();
  const account = $('#account').value.trim();
  const destination = $('#destination').value.trim();
  if (!state || !account || (selectedMode === 'ssh' && !destination)) {
    $('#connection-feedback').textContent = '请填写状态目录、账号别名，以及 SSH 模式下的连接目标。';
    $('#connection-feedback').classList.add('error'); return;
  }
  connectionDirty = false;
  connection = { mode: selectedMode, destination: selectedMode === 'ssh' ? destination : null, runtime_binary: $('#runtime-binary').value.trim() || null, state, account };
  $('#connection-summary').textContent = selectedMode === 'ssh' ? `通过 SSH 控制 ${destination}` : '控制这台电脑上的 Runtime';
  $('#connection-feedback').textContent = connection.mode === 'ssh' ? '使用系统 SSH 密钥与 known_hosts；首次连接请先在终端确认主机。' : '配置已应用。连接本机 Runtime，不需要 SSH。';
  $('#connection-feedback').classList.remove('error');
  for (const platform of platforms) Object.assign(states.get(platform.id), { status: platform.paused ? 'paused' : 'unknown', detail: platform.description, displayName:null, checked: false, error: null });
  note(selectedMode === 'ssh' ? '已选择 SSH 控制通道' : '已选择本地控制通道');
  window.dispatchEvent(new Event('operator:connection'));
  await refreshAll();
}

restoreAppearance();
for (const kind of Object.keys(appearanceOptions)) {
  document.querySelectorAll(`[data-${kind}-choice]`).forEach((button) => button.addEventListener('click', () => setAppearance(kind, button.dataset[`${kind}Choice`])));
}
$('#connection-form').addEventListener('submit', applyConnection);
$('#connection-form').addEventListener('input', markConnectionDirty);
document.querySelectorAll('.segment').forEach((button) => button.addEventListener('click', () => setMode(button.dataset.mode)));
$('#refresh-all').addEventListener('click', refreshAll);
$('#cancel-active').addEventListener('click', cancelLogin);
$('#input-form').addEventListener('submit', submitInput);
$('#x-window-open').addEventListener('click', openXWindow);
$('#x-browser-open').addEventListener('click', openXBrowser);
document.querySelectorAll('[data-cookie-mode]').forEach((button) => button.addEventListener('click', () => setCookieInputMode(button.dataset.cookieMode)));
$('#x-import-details').addEventListener('toggle', () => {
  if (requiredInput?.kind === 'cookies' && !$('#x-import-details').open) clearPrivateFields();
  syncCookieImport();
});
for (const selector of ['#input-close', '#input-cancel']) $(selector).addEventListener('click', cancelLogin);
$('#input-dialog').addEventListener('cancel', (event) => { event.preventDefault(); cancelLogin(); });
window.addEventListener('pagehide', clearPrivateFields);
$('#memory-toggle').addEventListener('click', toggleMemory);
document.addEventListener('visibilitychange', () => {
  clearTimeout(memoryTimer); memoryTimer = null;
  if (document.hidden) $('#memory-state').textContent = memoryPaused ? '已暂停' : '后台暂停';
  else if (!memoryPaused) sampleMemory();
});
window.addEventListener('pagehide', () => { memoryDisposed = true; clearTimeout(memoryTimer); memoryTimer = null; });
window.addEventListener('operator:view',()=>{clearTimeout(memoryTimer);memoryTimer=null;if(window.GlimlinkPage==='settings')sampleMemory();});
window.addEventListener('operator:language',render);
window.addEventListener('operator:connection',()=>{memoryGeneration++;clearMemory('等待当前 Runtime 的实际采样。');sampleMemory();});
window.addEventListener('pageshow', () => { memoryDisposed = false; sampleMemory(); });

async function initialize() {
  if (!bridge) {
    $('#preview-banner').hidden = false;
    const badge = $('#bridge-badge'); badge.className = 'bridge-badge preview';
    badge.replaceChildren(element('i'), document.createTextNode('界面预览'));
    $('#connection-feedback').textContent = '普通浏览器预览不会连接 Runtime 或发起账号登录。';
    $('#memory-toggle').disabled = true;
    clearMemory('界面预览未连接 Runtime，无法测量。');
    $('#memory-state').textContent = '预览不可用';
  } else {
    try {
      const preset = await tauri.core.invoke('launch_connection');
      if (preset) {
        connection = preset;
        setMode(preset.mode);
        $('#destination').value = preset.destination || '';
        $('#state').value = preset.state;
        $('#account').value = preset.account;
        $('#runtime-binary').value = preset.runtime_binary || '';
        connectionDirty = false;
        $('#connection-summary').textContent = preset.mode === 'ssh' ? `通过 SSH 控制 ${preset.destination}` : '控制这台电脑上的 Runtime';
        $('#connection-feedback').textContent = '已载入启动器的连接配置；尚未检查账号或发起登录。';
        window.dispatchEvent(new Event('operator:connection'));
      }
    } catch (_) { toast('无法读取启动器连接配置，请手动设置。'); }
    try { await tauri.event.listen('operator-login', (event) => handleLoginEvent(event.payload)); }
    catch (_) { toast('无法订阅登录状态，请重新打开操控端。'); }
  }
  render();
  sampleMemory();
}
initialize();
