// SPDX-License-Identifier: MIT
// Human control plane. Chat data never becomes markup or persistent browser data.
(() => {
  const q = (s) => document.querySelector(s);
  const t = (s) => window.GlimlinkI18n?.text(s) || s;
  const dataNode = (tag, cls, text) => { const n = element(tag, cls, String(text ?? '')); n.setAttribute('translate', 'no'); return n; };
  const ownNode = (tag, cls, text) => element(tag, cls, t(text));
  let page = 'connections';
  let snapshot = null;
  let currentAgent = 'main';
  let currentConversation = 'operator';
  let currentPersona = 'main';
  let lastConversation = null;
  let poll = null;
  let polling = false;
  let writes = false;
  let chain = Promise.resolve();
  let modelDirty = false;
  let modeDirty = false;
  let profileRevision = 0;
  let lastProfileAgent = null;
  let pendingMessage = null;
  let commandRevision = 0;
  let commandDirty = false;
  let connectionEpoch = 0;
  const titles = new Map();
  const expandedReplies=new Map();
  const pageNames = { connections: ['连接', '账号与 Runtime，各自保持连接。'], control: ['调控 · 会话', '和 AI 对话，选择引擎，调整它的节奏。'], commands: ['指令', '把管理权交给明确获准的会话。'], settings: ['设置', '外观、语言与这台操控端的状态。'] };
  const modeNames = { active: '活跃', quiet: '安静', paused: '暂停' };
  const jobNames = { pending: '排队中', running: '正在回复', completed: '已完成', needs_auth: '需要登录', failed: '未完成', interrupted: '已中断', canceled: '已取消' };
  const verbs = { help: '查看帮助', status: '查看状态', agent: '切换 Agent', model: '切换模型', enable: '启用 Agent', disable: '暂停 Agent', quiet: '只响应直接消息', wake: '唤醒自主活动' };
  function feedback(selector, message, error = false) { const n = q(selector); n.textContent = t(message); n.classList.toggle('error', error); }
  function errorText(e) { const code = typeof e === 'string' ? e : publicCode(e); const map = { profile_required: '请先为这个 Agent 保存引擎与模型。', revision_conflict: '配置已被其他会话修改，请重新读取后再保存。', invalid_message: '请输入有效消息，最多 16 KiB。', message_id_conflict: '这条消息的标识已有不同内容，请新建消息。', control_timeout: '请求超时，结果尚未确认。重试同一条消息会复用原标识。', control_unavailable: '无法连接调控通道，请检查 SSH 和远端版本。', worker_control_unavailable: '此部署尚未提供后台进程控制，请检查 Docker 部署。', invalid_command_scope: '请明确填写账号、会话与发送者，不支持通配授权。', login_busy: '请先完成或取消当前登录。' }; return t(map[code] || '操作未完成，请重新读取状态。') + ` (${code})`; }
  function serial(fn) { const p = chain.then(fn); chain = p.catch(() => {}); return p; }
  function request(command) { const destination={...connection};const epoch=connectionEpoch;return serial(async () => { if (!bridge || connectionDirty || epoch!==connectionEpoch) throw { code: 'control_unavailable' }; if (active) throw { code: 'login_busy' }; const v=await tauri.core.invoke('control_request', { connection:destination, command });if(epoch!==connectionEpoch)throw{code:'connection_changed'};return v; }); }
  async function mutate(command, message = '已应用') { if (writes) return; writes = true; try { const v = await request(command); feedback('#control-feedback', message); snapshot=await request({action:'snapshot'});renderState();if(page==='control')await loadConversation();return v; } catch (e) { feedback(page === 'commands' ? '#commands-feedback' : '#control-feedback', errorText(e), true); throw e; } finally { writes = false;if(snapshot)renderState(); } }
  function clearPoll() { clearTimeout(poll); poll = null; }
  function schedule() { clearPoll(); if (page === 'control' && !document.hidden) poll = setTimeout(() => refresh().catch(() => {}), 5000); }
  function setPage(next) {
    if (!pageNames[next]) return;
    page = next; window.GlimlinkPage = page;
    document.querySelectorAll('[data-page]').forEach((n) => { n.hidden = n.dataset.page !== page; });
    document.querySelectorAll('[data-view]').forEach((n) => { const selected = n.dataset.view === page; n.classList.toggle('selected', selected); if (selected) n.setAttribute('aria-current', 'page'); else n.removeAttribute('aria-current'); });
    q('#page-name').textContent = t(pageNames[page][0]); q('#page-description').textContent = t(pageNames[page][1]);
    q('#refresh-all').hidden = page !== 'connections';
    clearPoll(); window.dispatchEvent(new CustomEvent('operator:view', { detail: page }));
    if (page === 'control') refresh().catch(() => {});
    if (page === 'connections') loadIdentities().catch(() => {});
    if (page === 'commands') reloadCommands().catch(() => {});
  }
  async function loadIdentities(){const v=await request({action:'connections'});for(const item of v.identities||[]){if(item.account!==connection.account||!states.has(item.platform))continue;const state=states.get(item.platform);if(!state.checked&&item.status!=='signed_out'){state.displayName=item.name||null;state.status='saved_session';state.detail='上次读取的账号名称，核验后显示当前登录状态。';}}render();markBridgeAvailable();}
  function options(select, values, selected) { const signature = JSON.stringify(values); if (select.dataset.values !== signature) { select.replaceChildren(...values.map((v) => { const n = dataNode('option', '', v); n.value = v; return n; })); select.dataset.values = signature; } select.value = selected; }
  function agentNames() { return Array.from(new Set(['main', ...(snapshot?.agents || []), ...Object.keys(snapshot?.profiles || {})])).slice(0, 32); }
  function fillProfile(force = false) {
    const p = snapshot?.profiles?.[currentAgent];
    if (force || (!modelDirty && document.activeElement?.closest('#profile-form') === null)) { q('#agent-engine').value = p?.engine || 'codex'; q('#agent-model').value = p?.model || ''; profileRevision = p?.revision || 0; modelDirty = false; lastProfileAgent = currentAgent; }
    if (force || !modeDirty) { q('#agent-mode').value = snapshot?.preferences?.modes?.[currentAgent] || 'active'; modeDirty = false; }
  }
  function renderState() {
    if (!snapshot) return;
    const selected = snapshot.preferences?.selected_agent || 'main';
    if (selected !== currentAgent && !writes && !q('#profile-form').contains(document.activeElement)) { currentAgent = selected; modelDirty = false; modeDirty = false; }
    options(q('#control-agent'), agentNames(), currentAgent); fillProfile(lastProfileAgent !== currentAgent);
    const host = snapshot.host || {};
    const running = host.phase === 'running_job';
    q('#worker-state').textContent = t(snapshot.worker_online ? (running ? '正在运行一轮' : '后台运行中') : host.status === 'needs_auth' ? '后台需要登录' : '后台未运行');
    q('#worker-detail').textContent = t(snapshot.worker_online ? '可以继续聊天或调整后续使用的 Agent。' : '先完成订阅登录，再启动后台处理消息。');
    q('#worker-start').disabled = !bridge || snapshot.worker_online || writes;
    q('#worker-stop').disabled = !bridge || !snapshot.worker_online || writes;
    const ownJobs = (snapshot.jobs || []).filter((j) => j.context?.agent === currentAgent);
    const activeJob = ownJobs.find((j) => j.status === 'running');
    q('#agent-live').replaceChildren(ownNode('span', 'state-pill', modeNames[snapshot.preferences?.modes?.[currentAgent] || 'active']), ownNode('p', '', activeJob ? '当前轮结束后应用活跃状态变更。' : '当前没有正在运行的回复。'));
    q('#conversation-agent').textContent = `${currentAgent} · ${snapshot.profiles?.[currentAgent]?.engine || '—'}`;
    renderConversations(); renderActivities();
  }
  function renderConversations() {
    const map = new Map();
    for (const c of snapshot?.conversations || []) map.set(`${c.persona}/${c.conversation}`, c);
    for (const j of snapshot?.jobs || []) if (j.context?.conversation) map.set(`${j.context.persona}/${j.context.conversation}`, { persona: j.context.persona, conversation: j.context.conversation });
    map.set(`${currentPersona}/${currentConversation}`, { persona: currentPersona, conversation: currentConversation });
    const list = Array.from(map.values()).slice(-128).reverse();
    q('#conversation-list').replaceChildren(...list.map((c) => { const b = document.createElement('button'); b.type = 'button'; b.className = 'conversation-item'; b.classList.toggle('selected', c.persona === currentPersona && c.conversation === currentConversation); b.append(dataNode('strong', '', titles.get(`${c.persona}/${c.conversation}`) || c.conversation), dataNode('span', '', c.persona)); b.addEventListener('click', () => { currentPersona = c.persona; currentConversation = c.conversation; pendingMessage = null; renderConversations(); loadConversation().catch(showError); }); return b; }));
    q('#conversation-title').textContent = titles.get(`${currentPersona}/${currentConversation}`) || currentConversation;
  }
  async function loadConversation() { const persona=currentPersona,conversation=currentConversation;const value = await request({ action: 'conversation', persona, conversation });if(persona!==currentPersona||conversation!==currentConversation)return;lastConversation = value; renderMessages(value); }
  function showError(e) { feedback('#control-feedback', errorText(e), true); }
  function renderMessages(value) {
    const rows = [...(value.history?.turns || []).map((v) => ({ id: v.job_id, user: v.user, assistant: v.assistant, status: v.status, updated_at: v.at, context: { agent: '—' } })), ...(value.jobs || [])];
    const unique = new Map(rows.map((r) => [r.id, r])); const ordered = [...unique.values()].sort((a, b) => (a.created_at || a.updated_at) - (b.created_at || b.updated_at)).slice(-20);
    const nodes = [];
    for (let row of ordered) {
      const full=expandedReplies.get(row.id);if(full&&full.status===row.status&&full.updated_at===row.updated_at)row={...row,...full,truncated:false};
      const article = document.createElement('article'); article.className = 'chat-turn';
      article.append(dataNode('div', 'chat-user', row.user), ownNode('span', `job-status job-${row.status}`, jobNames[row.status] || row.status));
      if (row.assistant) article.append(dataNode('div', 'chat-assistant', row.assistant));
      if (row.error) article.append(dataNode('p', 'chat-error', row.error));
      if (row.truncated) { const more = ownNode('button', 'small-button', '展开完整回复'); more.type = 'button'; more.addEventListener('click', async () => { try { const full = await request({ action: 'job', id: row.id });expandedReplies.set(row.id,full);if(expandedReplies.size>4)expandedReplies.delete(expandedReplies.keys().next().value);const target = (lastConversation?.jobs || []).find((j) => j.id === row.id); if (target) Object.assign(target, full,{truncated:false}); renderMessages(lastConversation); } catch (e) { showError(e); } }); article.append(more); }
      if (row.status === 'pending') { const cancel = ownNode('button', 'text-button', '取消排队'); cancel.type = 'button'; cancel.addEventListener('click', () => mutate({ action: 'cancel', id: row.id }).catch(() => {})); article.append(cancel); }
      article.append(dataNode('span', 'turn-meta', [row.context?.agent, row.model || row.engine].filter(Boolean).join(' · '))); nodes.push(article);
    }
    if (!nodes.length) nodes.push(ownNode('p', 'empty-state', '这里还没有消息。开始一段对话吧。'));
    if (value.history?.truncated || value.bounded) nodes.push(ownNode('p', 'history-note', '显示 Runtime 保留的近期会话，较早消息可能已按保留策略清理。'));
    const box = q('#chat-messages'); const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    const signature = JSON.stringify(ordered.map((r) => [r.id, r.status, r.assistant, r.error, r.truncated]));
    if (box.dataset.rendered !== signature) { box.replaceChildren(...nodes); box.dataset.rendered = signature; if (atBottom) box.scrollTop = box.scrollHeight; }
  }
  function renderActivities() { const list = snapshot?.activities?.activities || []; q('#activity-controls').replaceChildren(...(list.length ? list.map((a) => { const n = document.createElement('div'); n.className = 'activity-control'; n.append(dataNode('strong', '', a.config.id), ownNode('span', '', a.config.enabled ? '已启用' : '未启用')); const toggle = ownNode('button', 'small-button', a.config.enabled ? '关闭活动' : '启用活动'); toggle.type = 'button'; toggle.addEventListener('click', () => mutate({ action: 'activity', id: a.config.id, enabled: !a.config.enabled, wake: false }).catch(() => {})); const wake = ownNode('button', 'text-button', '立即唤醒'); wake.type = 'button'; wake.addEventListener('click', () => mutate({ action: 'activity', id: a.config.id, enabled: true, wake: true }).catch(() => {})); n.append(toggle, wake); return n; }) : [ownNode('p', 'panel-help', '尚未配置自主活动。')])); }
  async function refresh() {
    if (polling || page !== 'control' || document.hidden) return;
    polling = true;
    try { snapshot = await request({ action: 'snapshot' }); markBridgeAvailable(); renderState(); await loadConversation(); }
    catch (e) { showError(e); }
    finally { polling = false; schedule(); }
  }
  async function worker(start) { if (writes) return; writes = true; try { await serial(() => tauri.core.invoke('worker_control', { connection, start })); feedback('#control-feedback', start ? '已请求启动后台。' : '后台已停止，账号与会话保留。'); } catch (e) { showError(e); } finally { writes = false; await refresh(); } }
  function field(label, value, kind = 'text') { const wrapper = document.createElement('label'); wrapper.className = 'field'; wrapper.append(ownNode('span', '', label)); const input = document.createElement('input'); input.type = kind; input.value = value; input.autocomplete = 'off'; input.spellcheck = false; wrapper.append(input); return { wrapper, input }; }
  function ruleCard(b) {
    const card = document.createElement('fieldset'); card.className = 'command-binding surface-card'; card.dataset.id = b.id;
    const legend = dataNode('legend', '', b.id); card.append(legend);
    const refs = {}; const grid = document.createElement('div'); grid.className = 'command-fields';
    const fields = [['account', '登录档案别名'], ['sender_id', '允许的 QQ 发送者'], ['chat_id', '会话标识'], ['prefix', '指令前缀'], ['agent', '初始 Agent']];
    for (const [key, label] of fields) { const f = field(label, b[key]); f.input.dataset.key = key; f.input.maxLength = key === 'prefix' ? 24 : 128; refs[key] = f.input; grid.append(f.wrapper); }
    const typeLabel = document.createElement('label'); typeLabel.className = 'field'; typeLabel.append(ownNode('span', '', '会话类型')); const type = document.createElement('select'); for (const [v, label] of [['private', '指定用户的私聊'], ['group', '指定群与指定发送者']]) { const o = ownNode('option', '', label); o.value = v; type.append(o); } type.value = b.chat_type; typeLabel.append(type); grid.prepend(typeLabel); refs.chat_type = type;
    const langLabel = document.createElement('label'); langLabel.className = 'field'; langLabel.append(ownNode('span', '', '指令回执语言')); const lang = document.createElement('select'); for (const [v, label] of [['en', 'English'], ['zh-CN', '简体中文']]) { const o = element('option', '', label); o.value = v; lang.append(o); } lang.value = b.language; langLabel.append(lang); grid.append(langLabel); refs.language = lang;
    card.append(ownNode('span', 'section-chip', 'QQ'), grid);
    const enabled = document.createElement('input'); enabled.type = 'checkbox'; enabled.checked = b.enabled; const enLabel = document.createElement('label'); enLabel.className = 'toggle-label'; enLabel.append(enabled, ownNode('span', '', '启用这条授权'));
    const reply = document.createElement('input'); reply.type = 'checkbox'; reply.checked = b.reply; const reLabel = document.createElement('label'); reLabel.className = 'toggle-label'; reLabel.append(reply, ownNode('span', '', '向原会话发送执行回执'));
    const permissions = document.createElement('div'); permissions.className = 'command-permissions'; for (const [verb, label] of Object.entries(verbs)) { const l = document.createElement('label'); const c = document.createElement('input'); c.type = 'checkbox'; c.value = verb; c.dataset.verb = verb; c.checked = b.allowed.includes(verb); l.append(c, ownNode('span', '', label)); permissions.append(l); }
    const remove = ownNode('button', 'text-button', '移除此授权'); remove.type = 'button'; remove.addEventListener('click', () => { card.remove(); commandDirty = true; feedback('#commands-feedback', '有未保存的指令配置。'); });
    refs.sender_id.addEventListener('input', () => { if (type.value === 'private') refs.chat_id.value = refs.sender_id.value; });
    card.append(enLabel, reLabel, ownNode('h3', '', '允许的管理操作'), permissions, ownNode('p', 'panel-help', '指令只管理 Runtime，不用于给 AI 发自然语言任务。'), remove);
    card.readBinding = () => ({ id: b.id, platform: 'qq', enabled: enabled.checked, reply: reply.checked, chat_type: type.value, language: lang.value, ...Object.fromEntries(fields.map(([key]) => [key, refs[key].value.trim()])), allowed: Array.from(permissions.querySelectorAll('input:checked')).map((n) => n.value) });
    card.addEventListener('input', () => { commandDirty = true; feedback('#commands-feedback', '有未保存的指令配置。'); }); return card;
  }
  function renderCommands(v) { commandRevision = v.revision; commandDirty = false; q('#commands-enabled').checked = v.config.enabled; q('#command-bindings').replaceChildren(...v.config.bindings.map(ruleCard)); q('#command-receipts').replaceChildren(...(v.receipts.length ? v.receipts.map((r) => dataNode('p', '', `${r.binding} · ${r.verb} · ${r.status}`)) : [ownNode('p', 'panel-help', '还没有收到获准的指令。')])); feedback('#commands-feedback', 'QQ 实时投递尚未完成实号验收；规则默认关闭。'); }
  async function reloadCommands() { if (commandDirty) return; try { renderCommands(await request({ action: 'commands_get' })); markBridgeAvailable(); } catch (e) { feedback('#commands-feedback', errorText(e), true); } }
  document.querySelectorAll('[data-view]').forEach((b) => b.addEventListener('click', () => setPage(b.dataset.view)));
  q('#settings-memory-host').append(q('.memory-card')); q('#settings-log-host').append(q('#activity-card'));
  const agentPanel=q('.agent-control'),controlLayout=q('.control-layout'),drawer=q('#agent-settings-dialog'),compact=matchMedia('(max-width:1250px)');
  function positionAgentPanel(){if(compact.matches){q('#agent-settings-body').append(agentPanel);q('#agent-controls-open').hidden=false;}else{if(drawer.open)drawer.close();controlLayout.append(agentPanel);q('#agent-controls-open').hidden=true;}}
  compact.addEventListener('change',positionAgentPanel);positionAgentPanel();
  q('#agent-controls-open').addEventListener('click',()=>{if(!drawer.open)drawer.showModal();});q('#agent-controls-close').addEventListener('click',()=>drawer.close());
  q('#control-refresh').addEventListener('click', () => refresh().catch(() => {}));
  q('#worker-start').addEventListener('click', () => worker(true)); q('#worker-stop').addEventListener('click', () => worker(false));
  q('#conversation-new').addEventListener('click', () => { currentConversation = `chat-${Date.now().toString(36)}`; currentPersona = 'main'; titles.set(`${currentPersona}/${currentConversation}`, t('新会话')); pendingMessage = null; renderConversations(); loadConversation().catch(showError); });
  q('#control-agent').addEventListener('change', async (e) => { currentAgent = e.target.value; modelDirty = false; modeDirty = false; try { await mutate({ action: 'select', agent: currentAgent }); fillProfile(true); } catch (_) {} });
  q('#profile-form').addEventListener('input', () => { modelDirty = true; }); q('#mode-form').addEventListener('input', () => { modeDirty = true; });
  q('#agent-engine').addEventListener('change',()=>{q('#agent-model').value=q('#agent-engine').value==='antigravity'?'gemini-3.8-flash-medium':'gpt-6.1-terra';modelDirty=true;});
  q('#profile-form').addEventListener('submit', async (e) => { e.preventDefault(); try { await mutate({ action: 'profile', agent: currentAgent, engine: q('#agent-engine').value, model: q('#agent-model').value.trim(), expected_revision: profileRevision }); modelDirty = false; fillProfile(true); } catch (_) {} });
  q('#mode-form').addEventListener('submit', async (e) => { e.preventDefault(); try { await mutate({ action: 'mode', agent: currentAgent, mode: q('#agent-mode').value }); modeDirty = false; fillProfile(true); } catch (_) {} });
  q('#agent-create').addEventListener('click', async () => { const id = q('#agent-new-name').value.trim(); if (!/^[A-Za-z0-9_:-]{1,64}$/.test(id)) { feedback('#control-feedback', 'Agent 标识使用字母、数字、下划线、冒号或短横线。', true); return; } try { await mutate({ action: 'profile', agent: id, engine: q('#agent-engine').value, model: q('#agent-model').value.trim(), expected_revision: 0 }); await mutate({ action: 'select', agent: id }); q('#agent-new-name').value = ''; currentAgent = id; fillProfile(true); } catch (_) {} });
  q('#chat-form').addEventListener('submit', async (e) => { e.preventDefault(); if (writes) return; const text = q('#chat-input').value; if (!text.trim()) return; const key = JSON.stringify([currentAgent, currentPersona, currentConversation, text]); if (pendingMessage?.key !== key) { const bytes = crypto.getRandomValues(new Uint8Array(16)); pendingMessage = { key, id: 'message-' + [...bytes].map((n) => n.toString(16).padStart(2, '0')).join('') }; } try { const value = await mutate({ action: 'send', id: pendingMessage.id, agent: currentAgent, persona: currentPersona, conversation: currentConversation, text }, '消息已排队。'); if (!value) return; titles.set(`${currentPersona}/${currentConversation}`, text.slice(0, 36)); q('#chat-input').value = ''; pendingMessage = null; if (!snapshot?.worker_online) feedback('#control-feedback', '消息已排队，启动后台后处理。'); renderConversations(); } catch (_) {} });
  q('#commands-enabled').addEventListener('change', () => { commandDirty = true; });
  q('#command-add').addEventListener('click', () => { if (q('#command-bindings').children.length >= 8) return; q('#command-bindings').append(ruleCard({ id: 'rule-' + Date.now().toString(36), enabled: false, platform: 'qq', account: connection.account || 'main', chat_type: 'private', chat_id: '', sender_id: '', prefix: '/gl', agent: currentAgent, allowed: ['help', 'status'], reply: false, language: window.GlimlinkI18n?.language() || 'en' })); commandDirty = true; });
  q('#commands-reload').addEventListener('click', () => { commandDirty = false; reloadCommands(); });
  q('#commands-save').addEventListener('click', async () => { const bindings = Array.from(q('#command-bindings').children).map((n) => n.readBinding()); try { const result = await request({ action: 'commands_set', config: { enabled: q('#commands-enabled').checked, bindings }, expected_revision: commandRevision }); renderCommands(result); feedback('#commands-feedback', '指令配置已保存，不会启动登录。'); } catch (e) { feedback('#commands-feedback', errorText(e), true); } });
  document.addEventListener('visibilitychange', () => { if (document.hidden) clearPoll(); else schedule(); });
  window.addEventListener('pagehide', clearPoll);
  window.addEventListener('operator:language', () => { setPage(page); if (snapshot) renderState(); if (lastConversation) renderMessages(lastConversation); });
  window.addEventListener('operator:connection', () => { connectionEpoch++; snapshot = null; lastProfileAgent = null; lastConversation = null; pendingMessage=null;expandedReplies.clear();titles.clear();if (page === 'control') refresh();if(page==='connections')loadIdentities().catch(()=>{}); });
  window.GlimlinkConsole = { setPage, refresh };
  setPage('connections');
})();
