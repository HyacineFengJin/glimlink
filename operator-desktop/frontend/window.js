// SPDX-License-Identifier: MIT
// Local window chrome; it never sends a Runtime request.
(() => {
  const bar=document.getElementById('window-titlebar');
  const buttons=Array.from(bar.querySelectorAll('button'));
  const maximize=document.getElementById('window-maximize');
  let timer=null;
  const invoke=window.__TAURI__?.core?.invoke;
  async function action(value) {
    if(!invoke)return;
    try {
      const state=await invoke('window_action',{action:value});
      if(typeof state?.maximized==='boolean') {
        maximize.dataset.maximized=String(state.maximized);
        const label=window.GlimlinkI18n?.text(state.maximized?'还原窗口':'最大化') || (state.maximized?'还原窗口':'最大化');
        maximize.title=label;maximize.setAttribute('aria-label',label);
      }
      if(typeof state?.can_minimize==='boolean')document.getElementById('window-minimize').hidden=!state.can_minimize;
    } catch(_) {document.getElementById('window-feedback').textContent=window.GlimlinkI18n?.text('窗口操作未完成。') || '窗口操作未完成。';}
  }
  if(!invoke){buttons.forEach(b=>b.disabled=true);return;}
  bar.addEventListener('mousedown',e=>{
    if(e.button!==0 || e.target.closest('.window-controls'))return;
    e.preventDefault();window.GlimlinkSelects?.close();
    action(e.detail===2?'toggle_maximize':'drag');
  });
  document.getElementById('window-minimize').addEventListener('click',()=>action('minimize'));
  maximize.addEventListener('click',()=>action('toggle_maximize'));
  document.getElementById('window-close').addEventListener('click',()=>action('close'));
  window.addEventListener('resize',()=>{clearTimeout(timer);timer=setTimeout(()=>action('state'),100);});
  window.addEventListener('operator:language',()=>action('state'));
  action('state');
})();
