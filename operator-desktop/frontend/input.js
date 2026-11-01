// SPDX-License-Identifier: MIT
// Text input geometry belongs to the operator, never Runtime or model context.
// WebKitGTK can report document coordinates to IME after a document scroll.
// Measure the actual caret, not the textarea's box or a desktop-specific offset.
(() => {
  const invoke=window.__TAURI__?.core?.invoke;
  if(!invoke)return;
  const properties=['box-sizing','font-family','font-size','font-style','font-weight','font-stretch','font-variant','font-kerning','font-feature-settings','font-variation-settings','line-height','letter-spacing','word-spacing','text-indent','text-align','text-transform','text-rendering','tab-size','direction','word-break','line-break','overflow-wrap','padding-top','padding-right','padding-bottom','padding-left','border-top-width','border-right-width','border-bottom-width','border-left-width','border-style'];
  let enabled=false,frame=0,host,mirror,textNode,observed,inFlight=false,pending=null,last='',revision=0;
  const resize=new ResizeObserver(()=>schedule());
  const editor=()=>{const n=document.activeElement;return n instanceof HTMLTextAreaElement || (n instanceof HTMLInputElement && ['text','search','url','tel'].includes(n.type)) ? n : null;};
  function initialize() {
    host=document.createElement('div');host.setAttribute('aria-hidden','true');host.setAttribute('translate','no');host.inert=true;
    host.style.cssText='position:fixed;left:0;top:0;width:0;height:0;visibility:hidden;pointer-events:none;contain:layout style;';
    // A shadow tree keeps translation/select mutation observers out of the
    // ephemeral measuring text. Wipe that text before returning each measure.
    mirror=document.createElement('div');host.attachShadow({mode:'closed'}).append(mirror);document.body.append(host);
  }
  function measure(n) {
    const r=n.getBoundingClientRect(),s=getComputedStyle(n);
    if(!r.width || !r.height || s.writingMode!=='horizontal-tb' || n.value.length>65536)return null;
    const position=n.selectionDirection==='backward'?n.selectionStart:n.selectionEnd;
    if(position===null)return null;
    if(!host)initialize();
    mirror.style.cssText='position:fixed;left:0;top:0;margin:0;visibility:hidden;overflow:visible;';
    for(const key of properties)mirror.style.setProperty(key,s.getPropertyValue(key));
    const borderX=parseFloat(s.borderLeftWidth)+parseFloat(s.borderRightWidth);
    mirror.style.boxSizing='border-box';mirror.style.width=(n.clientWidth+borderX)+'px';
    mirror.style.whiteSpace=n.tagName==='TEXTAREA'&&n.wrap!=='off'?'pre-wrap':'pre';
    if(n.tagName==='TEXTAREA')mirror.style.overflowWrap='break-word';
    textNode=document.createTextNode(n.value+'\u200b');mirror.append(textNode);
    try {
      const range=document.createRange();range.setStart(textNode,position);range.collapse(true);
      const caret=range.getBoundingClientRect(),box=mirror.getBoundingClientRect();
      if(!caret.height)return null;
      const scaleX=r.width/n.offsetWidth,scaleY=r.height/n.offsetHeight;
      let top=caret.top-box.top;
      if(n.tagName==='INPUT')top+=(n.clientHeight-parseFloat(s.paddingTop)-parseFloat(s.paddingBottom)-(parseFloat(s.lineHeight)||caret.height))/2;
      const x=r.left+(caret.left-box.left-n.scrollLeft)*scaleX;
      const y=r.top+(top-n.scrollTop)*scaleY;
      const height=caret.height*scaleY;
      // A caret clipped by the editor or viewport must not put a popup at an
      // unrelated edge. Native focus-out still belongs to WebKit, not this API.
      if(x<Math.max(0,r.left) || x>=Math.min(innerWidth,r.right) || y<Math.max(0,r.top) || y>=Math.min(innerHeight,r.bottom))return null;
      return {x,y,width:Math.max(1,caret.width*scaleX),height,viewport_width:innerWidth,viewport_height:innerHeight};
    } finally {mirror.replaceChildren();textNode=null;}
  }
  async function drain() {
    if(inFlight || !pending)return;
    const next=pending;pending=null;inFlight=true;
    try {await invoke('ime_cursor_area',{area:next.area});} catch(_) {last='';}
    finally {inFlight=false;if(pending?.revision===revision)drain();else pending=null;}
  }
  function update() {
    frame=0;if(!enabled || document.hidden || !document.hasFocus())return;
    const n=editor();
    if(n!==observed){resize.disconnect();observed=n;last='';revision++;if(n)resize.observe(n);}
    if(!n){pending=null;return;}
    const area=measure(n);if(!area)return;
    const signature=JSON.stringify(area);if(signature===last)return;last=signature;
    pending={area,revision};drain();
  }
  function schedule(){last='';if(enabled&&!frame)frame=requestAnimationFrame(update);}
  for(const name of ['focusin','focusout','input','keyup','pointerup','select','selectionchange','compositionstart','compositionupdate','compositionend','scroll'])document.addEventListener(name,schedule,true);
  window.addEventListener('resize',schedule);window.addEventListener('focus',()=>{last='';schedule();});
  window.addEventListener('blur',()=>{pending=null;last='';revision++;});
  window.visualViewport?.addEventListener('resize',schedule);window.visualViewport?.addEventListener('scroll',schedule);
  window.addEventListener('operator:language',schedule);window.addEventListener('operator:appearance',schedule);
  invoke('ime_capabilities').then(value=>{enabled=value===true;schedule();}).catch(()=>{});
  document.fonts?.ready.then(schedule);
})();
