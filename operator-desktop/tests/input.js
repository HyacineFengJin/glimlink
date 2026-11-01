// SPDX-License-Identifier: MIT
// Actual WebKit layout; geometry assertions are separate from physical IME QA.
(async()=>{
 const results={},wait=ms=>new Promise(r=>setTimeout(r,ms)),assert=(v,m)=>{if(!v)throw Error(m)},near=(a,b)=>Math.abs(a-b)<2;
 const calls=()=>window.__calls.filter(c=>c.command==='ime_cursor_area');
 const sample=async(fn)=>{const before=calls().length;fn();for(let i=0;i<100&&calls().length===before;i++)await wait(20);assert(calls().length>before,'geometry update arrives');await wait(30);return calls().at(-1)?.args.area;};
 const changed=(n,value,pos)=>sample(()=>{n.value=value;n.setSelectionRange(pos,pos);n.focus();n.dispatchEvent(new Event('input',{bubbles:true}));});
 let fixture;
 try{
  // WebKit's first frame can follow GTK activation by up to one second.
  await new Promise(resolve=>requestAnimationFrame(resolve));await wait(1100);
  fixture=document.createElement('div');fixture.style.cssText='position:fixed;left:70px;top:120px;z-index:2000;width:300px;background:white;';
  const input=document.createElement('textarea');input.style.cssText='box-sizing:border-box;width:260px;height:130px;font:20px/30px monospace;letter-spacing:0;padding:10px;border:2px solid black;resize:none;';fixture.append(input);document.body.append(fixture);
  let area=await changed(input,'abc',0),start={...area},r=input.getBoundingClientRect();
  assert(area&&near(area.x,r.left+12)&&near(area.y,r.top+12+3),'caret starts at border + padding + font metrics: '+JSON.stringify({area,rect:[r.x,r.y,r.width,r.height]}));
  area=await changed(input,'abc',3);assert(area.x>start.x+25&&near(area.y,start.y),'caret moves horizontally');
  area=await changed(input,'first\nsecond',10);assert(near(area.y,start.y+30),'newline advances by actual line-height');
  results.caret_tracks_selection_and_multiline_text=true;
  const value='中文🙂 mixed text '.repeat(3);area=await changed(input,value,value.length);
  assert(area.y>start.y+30,'CJK/emoji and mixed text wrap');
  area=await changed(input,'line\n'.repeat(12),12);area=await sample(()=>{input.scrollTop=0;input.dispatchEvent(new Event('scroll'));});
  const bottom={...area};area=await sample(()=>{input.scrollTop=30;input.dispatchEvent(new Event('scroll'));});
  assert(near(area.y,bottom.y-30),'internal textarea scrolling');results.wrap_and_internal_scroll=true;
  input.scrollTop=0;await changed(input,'abc',3);const before=calls().at(-1).args.area;
  area=await sample(()=>{fixture.style.top='240px';window.dispatchEvent(new Event('resize'));});assert(near(area.y,before.y+120),'layout movement uses viewport coordinates');
  const spacer=document.createElement('div');spacer.style.height='3000px';document.body.append(spacer);fixture.style.position='absolute';area=await sample(()=>window.scrollTo(0,100));
  assert(near(area.y,before.y+20),'document scroll is subtracted once');window.scrollTo(0,0);spacer.remove();fixture.style.position='fixed';
  results.document_scroll_and_layout_change=true;
  const unscaled=calls().at(-1).args.area;fixture.style.transformOrigin='top left';area=await sample(()=>{fixture.style.transform='scale(1.25)';window.dispatchEvent(new Event('resize'));});const rect=input.getBoundingClientRect();assert(near(area.x,rect.left+(unscaled.x-70)*1.25),'editor scaling preserves caret offset');assert(near(area.height,unscaled.height*1.25),'scaled caret height');fixture.style.transform='';results.editor_scale_is_applied_once=true;
  const count=calls().length;input.blur();await wait(100);document.dispatchEvent(new Event('selectionchange'));await wait(60);assert(calls().length===count,'blur does not keep a native update loop');
  const password=document.createElement('input');password.type='password';password.value='synthetic-secret';fixture.append(password);password.focus();await wait(100);assert(calls().length===count,'password fields use native IME only');
  assert(calls().every(c=>Object.keys(c.args).join(',')==='area'&&Object.values(c.args.area).every(Number.isFinite)),'IPC carries numeric geometry only');
  assert(!JSON.stringify(calls()).includes('synthetic-secret')&&!JSON.stringify(calls()).includes(value),'no input text in geometry IPC');results.geometry_only_and_no_background_poll=true;
  const attrs=[];const observer=new MutationObserver(rows=>attrs.push(...rows));observer.observe(document.documentElement,{attributes:true,attributeFilter:['lang']});window.GlimlinkI18n.refresh();window.GlimlinkI18n.refresh();await wait(100);observer.disconnect();assert(attrs.length===0,'localization never rewrites unchanged input language');results.stable_document_language=true;
 }catch(e){results.error=e.message+' '+JSON.stringify({focus:document.hasFocus(),calls:calls().slice(-3),hidden:document.hidden,errors:window.__errors,style:fixture?.firstChild&&getComputedStyle(fixture.firstChild).writingMode,active:document.activeElement?.tagName,host:!!document.querySelector('[inert]'),scripts:[...document.scripts].map(n=>n.src)})}
 fixture?.remove();
 document.body.replaceChildren(Object.assign(document.createElement('pre'),{id:'verification-result',textContent:JSON.stringify(results)}));
})();
