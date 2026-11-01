// SPDX-License-Identifier: MIT
// Small accessible menus; native selects remain the source of form values.
(() => {
  const controls = new Map();
  let opened = null, sequence = 0, queued = false;
  function close(focus = false) {
    if (!opened) return;
    const c = opened; opened = null;
    c.menu.remove(); c.button.setAttribute('aria-expanded', 'false');
    c.button.removeAttribute('aria-activedescendant');
    if (focus && c.button.isConnected) c.button.focus();
  }
  function sync(c) {
    const text = c.select.selectedOptions[0]?.textContent || '—';
    if (c.label.textContent !== text) c.label.textContent = text;
    c.button.disabled = c.select.disabled;
    c.button.setAttribute('aria-label', c.select.getAttribute('aria-label') || c.select.closest('.field')?.querySelector('span')?.textContent || text);
    if (c.select.selectedOptions[0]?.getAttribute('translate') === 'no') c.label.setAttribute('translate', 'no');
    else c.label.removeAttribute('translate');
  }
  function highlight(c, index) {
    if (!c.items.length) return;
    c.index = Math.max(0, Math.min(index, c.items.length - 1));
    c.items.forEach((n, i) => n.classList.toggle('menu-active', i === c.index));
    c.button.setAttribute('aria-activedescendant', c.items[c.index].id);
    c.items[c.index].scrollIntoView({block:'nearest'});
  }
  function choose(c, i) {
    const option = c.options[i]; if (!option || option.disabled) return;
    c.select.value = option.value;
    c.select.dispatchEvent(new Event('input', {bubbles:true}));
    c.select.dispatchEvent(new Event('change', {bubbles:true}));
    sync(c); close(true);
  }
  function open(c) {
    if (c.button.disabled) return;
    close(); opened = c; sync(c);
    c.options = Array.from(c.select.options).filter(o => !o.disabled && !o.hidden);
    c.items = c.options.map((o, i) => {
      const item = document.createElement('div');
      item.id = c.menu.id + '-' + i; item.className = 'm3-menu-option';
      item.setAttribute('role','option'); item.setAttribute('aria-selected', String(o.selected));
      if (o.getAttribute('translate') === 'no') item.setAttribute('translate','no');
      const label = document.createElement('span'); label.textContent = o.textContent;
      const check = document.createElement('span'); check.className = 'menu-check'; check.textContent = '✓'; check.setAttribute('aria-hidden','true');
      item.append(label,check); item.addEventListener('click', () => choose(c,i));
      item.addEventListener('pointermove', () => highlight(c,i)); return item;
    });
    c.menu.replaceChildren(...c.items);
    (c.select.closest('dialog[open]') || document.body).append(c.menu);
    const r = c.button.getBoundingClientRect(), margin = 10;
    const room = Math.max(100, Math.max(window.innerHeight-r.bottom, r.top)-margin*2);
    c.menu.style.maxHeight = Math.min(300,room) + 'px';
    c.menu.style.width = Math.min(r.width,window.innerWidth-margin*2) + 'px';
    c.menu.style.left = Math.max(margin,Math.min(r.left,window.innerWidth-r.width-margin)) + 'px';
    const h = c.menu.getBoundingClientRect().height;
    c.menu.style.top = (window.innerHeight-r.bottom >= h+margin ? r.bottom+6 : Math.max(margin,r.top-h-6)) + 'px';
    c.button.setAttribute('aria-expanded','true');
    highlight(c, Math.max(0,c.options.findIndex(o => o.selected)));
  }
  function attach(select) {
    if (controls.has(select)) return;
    const wrapper=document.createElement('div'); wrapper.className='m3-select';
    const button=document.createElement('button'); button.type='button'; button.className='m3-select-trigger';
    const label=document.createElement('span'); label.className='select-label';
    const arrow=document.createElement('span'); arrow.className='select-arrow'; arrow.setAttribute('aria-hidden','true');
    const menu=document.createElement('div'); menu.id='m3-menu-'+(++sequence); menu.className='m3-menu'; menu.setAttribute('role','listbox');
    button.setAttribute('role','combobox'); button.setAttribute('aria-haspopup','listbox'); button.setAttribute('aria-controls',menu.id); button.setAttribute('aria-expanded','false');
    select.before(wrapper); wrapper.append(select,button); button.append(label,arrow);
    select.hidden=true; select.setAttribute('aria-hidden','true'); select.tabIndex=-1;
    const c={select,button,label,menu,index:0,items:[],options:[]}; controls.set(select,c); sync(c);
    button.addEventListener('click', () => opened===c ? close() : open(c));
    select.addEventListener('change', () => sync(c)); select.addEventListener('input', () => sync(c));
    button.addEventListener('keydown', e => {
      if (['ArrowDown','ArrowUp','Enter',' '].includes(e.key)) {
        e.preventDefault();
        if(opened!==c){open(c);return;}
        if(e.key==='ArrowDown')highlight(c,(c.index+1)%Math.max(1,c.items.length));
        else if(e.key==='ArrowUp')highlight(c,(c.index+c.items.length-1)%Math.max(1,c.items.length));
        else choose(c,c.index);
      } else if(opened===c && ['Home','End'].includes(e.key)) {
        e.preventDefault();highlight(c,e.key==='Home'?0:c.items.length-1);
      } else if(e.key==='Escape' && opened===c) {e.preventDefault();e.stopPropagation();close(true);}
      else if(e.key==='Tab')close();
      else if(opened===c && e.key.length===1 && !e.ctrlKey && !e.metaKey) {
        const i=c.options.findIndex(o => o.textContent.toLocaleLowerCase().startsWith(e.key.toLocaleLowerCase()));
        if(i>=0){e.preventDefault();highlight(c,i);}
      }
    });
  }
  function refresh() {
    document.querySelectorAll('select').forEach(attach);
    for (const [select,c] of controls) {
      if(!select.isConnected){if(opened===c)close();controls.delete(select);}
      else sync(c);
    }
  }
  document.addEventListener('pointerdown', e => {
    if(opened && !opened.menu.contains(e.target) && !opened.button.contains(e.target))close();
  },true);
  document.addEventListener('scroll',e => {if(opened && e.target!==opened.menu && !opened.menu.contains(e.target))close();},true);
  window.addEventListener('resize',()=>close()); window.addEventListener('operator:view',()=>close());
  window.GlimlinkSelects={refresh,close}; refresh();
  new MutationObserver(() => {if(queued)return;queued=true;queueMicrotask(()=>{queued=false;refresh();});}).observe(document.body,{subtree:true,childList:true,characterData:true});
})();
