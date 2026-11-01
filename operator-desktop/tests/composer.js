// SPDX-License-Identifier: MIT
// Delayed and ambiguous delivery, with real WebKit keyboard/composition events.
(async()=>{
 const results={},q=s=>document.querySelector(s),wait=ms=>new Promise(r=>setTimeout(r,ms)),tick=()=>wait(90);
 const assert=(v,m)=>{if(!v)throw Error(m)};
 const enter=(extra={})=>{const e=new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true,...extra});q('#chat-input').dispatchEvent(e);return e;};
 const sends=()=>window.__calls.filter(c=>c.command==='control_request'&&c.args.command.action==='send');
 const release=()=>{window.__holdSends=false;(window.__releaseSends||[]).splice(0).forEach(fn=>fn());};
 try{
  await tick();q('[data-view=control]').click();await tick();await tick();
  const input=q('#chat-input');input.focus();window.__holdSends=true;
  input.value='First delayed hello';enter();
  assert(input.value==='','Enter clears synchronously');
  assert(q('#chat-messages').textContent.includes('First delayed hello'),'bubble before any network reply');
  assert(document.activeElement===input,'focus stays in composer');
  input.value='Second delayed hello';q('#chat-send').click();
  assert(input.value===''&&q('#chat-messages').textContent.includes('Second delayed hello'),'rapid second send is immediate');
  input.value='Third draft stays intact';await tick();release();await tick();await tick();await tick();
  assert(input.value==='Third draft stays intact','ack never erases next draft');
  assert(sends().length===2&&sends()[0].args.command.id!==sends()[1].args.command.id,'distinct immutable IDs');
  assert(sends()[0].args.command.id<sends()[1].args.command.id,'same-second IDs preserve queue order');
  assert(q('#chat-messages').querySelectorAll('.chat-user').length===2,'echo reconciles without duplicates');
  results.immediate_echo_clear_focus_and_rapid_messages=true;

  const first=q('[data-message-id]'),listItem=q('.conversation-item');
  await window.GlimlinkConsole.refresh();assert(first===q('[data-message-id]'),'unchanged message DOM is reused');
  assert(listItem===q('.conversation-item'),'unchanged conversation controls are reused');
  results.polling_preserves_dom_identity=true;

  input.value='line one';const newline=enter({shiftKey:true});assert(!newline.defaultPrevented&&input.value==='line one','Shift+Enter remains native newline');
  const before=sends().length;
  input.dispatchEvent(new CompositionEvent('compositionstart',{data:'ni'}));
  const comp=enter({isComposing:true});assert(!comp.defaultPrevented,'IME selection goes to native IME');
  q('#chat-form').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));
  assert(sends().length===before&&input.value==='line one','composition cannot submit');
  const calls=window.__calls.length;await window.GlimlinkConsole.refresh();assert(window.__calls.length===calls,'composition defers remote refresh');
  input.value='你好';input.dispatchEvent(new CompositionEvent('compositionend',{data:'你好'}));enter();
  assert(input.value==='你好','same commit Enter cannot send');
  await wait(140);enter();assert(input.value==='','next Enter sends committed Chinese');
  await tick();await tick();assert(sends().length===before+1,'Chinese submitted exactly once');
  results.enter_newline_and_ime_commit_are_distinct=true;

  window.__rejectSend=true;input.value='Recover this message';enter();await tick();await tick();
  const failed=[...q('#chat-messages').children].find(n=>n.textContent.includes('Recover this message'));
  assert(failed?.querySelector('.message-retry'),'uncertain delivery keeps retryable bubble');
  const failedId=sends().at(-1).args.command.id;
  input.value='A new draft';failed.querySelector('.message-retry').click();await tick();await tick();
  assert(sends().at(-1).args.command.id===failedId,'retry reuses operation ID');
  assert(input.value==='A new draft','retry preserves current draft');
  assert(window.__console.jobs.filter(j=>j.id===failedId).length===1,'retry enqueues once');
  results.failure_retains_text_and_retries_same_id=true;

  window.__loseSendAck=true;input.value='Accepted but response lost';enter();await tick();await tick();await tick();
  const lostId=sends().at(-1).args.command.id;
  assert(window.__console.jobs.filter(j=>j.id===lostId).length===1,'server accepted once');
  assert(q('[data-message-id="'+lostId+'"]')&&!q('[data-message-id="'+lostId+'"] .message-retry'),'canonical result resolves lost ACK');
  results.ambiguous_ack_reconciles_without_resending=true;

  input.value='Private draft one';const original=q('#conversation-title').textContent;
  q('#conversation-new').click();assert(input.value==='','new conversation has separate draft');
  input.value='Private draft two';
  const previous=[...q('#conversation-list').children].find(n=>n.textContent.includes(original));assert(previous,'original conversation retained');
  previous.click();assert(input.value==='Private draft one','switch restores original draft immediately');
  results.drafts_remain_scoped_to_conversations=true;
  const stored=JSON.stringify([...Array(localStorage.length)].map((_,i)=>[localStorage.key(i),localStorage.getItem(localStorage.key(i))]));
  assert(!stored.includes('Private draft')&&!stored.includes('Recover this message'),'messages and drafts are not browser-persisted');
  results.no_message_or_draft_browser_persistence=true;
 }catch(e){results.error=e.message}
 release();
 document.body.replaceChildren(Object.assign(document.createElement('pre'),{id:'verification-result',textContent:JSON.stringify(results)}));
})();
