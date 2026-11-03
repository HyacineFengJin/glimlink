// SPDX-License-Identifier: MIT
(async()=>{
 const results={},q=s=>document.querySelector(s),wait=ms=>new Promise(r=>setTimeout(r,ms)),assert=(v,m)=>{if(!v)throw Error(m)},tick=()=>wait(80);
 const event=(kind,data)=>window.__loginHandler({payload:{login_id:'synthetic-'+window.__job,platform:'x',kind,data}});
 try {
  await tick();q('[data-platform=x] .login-button').click();await tick();event('input_required',{kind:'cookies'});
  assert(document.activeElement===q('#x-browser-open'),'normal browser is the initial action');assert(q('#x-native-details').textContent.includes('Google / Apple'),'limited method explains SSO boundary');
  assert(!q('#x-import-details').open&&q('#input-submit').hidden&&q('#input-auth-token').disabled,'no unexplained cookie form or premature submit');
  q('#x-browser-open').click();await tick();assert(q('#x-import-details').open&&!q('#input-submit').hidden,'browser return has a visible next step');
  assert(q('#input-auth-token').required&&!q('#input-auth-token').disabled&&q('#cookie-header-field').hidden,'two-value guide is the default import');
  assert(q('#cookie-pair-guide').textContent.includes('Application')&&q('#cookie-pair-guide').textContent.includes('https://x.com')&&q('#cookie-pair-guide').textContent.includes('Value'),'instructions identify panel, origin, and values');results.login_and_import_are_distinct_complete_paths=true;
  q('#input-auth-token').value='synthetic-private-auth';q('#input-ct0').value='synthetic-private-csrf';q('#x-import-details').open=false;await tick();
  assert(!q('#input-auth-token').value&&!q('#input-ct0').value&&q('#input-auth-token').disabled&&q('#input-submit').hidden,'collapsing private entry clears and disables it');results.closing_import_clears_credentials=true;
  event('progress',{status:'verification_required',browser_handoff:true});await tick();assert(q('#x-import-details').open&&!q('#x-native-details').open,'external SSO handoff opens correct guide');
  q('[data-cookie-mode=header]').click();assert(!q('#cookie-header-guide').hidden&&q('#cookie-pair-guide').hidden,'advanced header has its own explanation');results.external_sso_and_advanced_help_have_visible_feedback=true;
  window.GlimlinkI18n.set('en');await tick();assert(q('#input-title').textContent==='Connect an X account'&&q('#cookie-header-guide').textContent.includes('What is a Cookie header?'),'guide switches to English');window.GlimlinkI18n.set('zh-CN');await tick();assert(q('#input-submit-label').textContent==='验证并连接','guide restores Chinese');results.guide_is_bilingual=true;
  event('finished',{status:'cancelled'});q('[data-platform=github] .login-button').click();await tick();assert(window.__calls.filter(c=>c.command==='begin_login').at(-1).args.method==='auto','GitHub main path uses default application and reuse');
  window.__loginHandler({payload:{login_id:'synthetic-'+window.__job,platform:'github',kind:'finished',data:{status:'cancelled'}}});assert(!q('[data-platform=github] .x-refresh-login'),'advanced token input is absent from normal cards');results.github_uses_default_authorization_without_extra_links=true;
 }catch(e){results.error=e.message}
 document.body.replaceChildren(Object.assign(document.createElement('pre'),{id:'verification-result',textContent:JSON.stringify(results)}));
})();
