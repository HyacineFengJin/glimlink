// SPDX-License-Identifier: MIT
// Synthetic browser APIs only. No browser profile, account or network is used.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const source=fs.readFileSync(require('node:path').join(__dirname,'../src/browser_login/extension/connect.js'),'utf8');
const tick=()=>new Promise(r=>setImmediate(r));
async function settle(){for(let i=0;i<16;i++)await tick()}
async function scenario({ready=true,grant=true,hash='12345/'+'a'.repeat(48),platform='pixiv',duplicate=false,next=false,click=true,expires=60,requestError=false,completeError=false,pendingGrant=false,pendingRevoke=false}={}){
 const events=[],nodes=Object.fromEntries(['connect','status','heading','target','scope'].map(id=>[id,{disabled:true,textContent:'',addEventListener:(_,f)=>nodes[id].click=f}]));
 const doc={documentElement:{dataset:{}},getElementById:id=>id==='__NEXT_DATA__'?{textContent:JSON.stringify({props:{pageProps:{isLoggedIn:ready,gaUserData:{userId:'42'}}}})}:nodes[id],querySelector:()=>next?null:({content:JSON.stringify({userData:ready?{id:'42',name:'synthetic'}:null})})};
 const cookie={name:'PHPSESSID',value:'synthetic-cookie',domain:'.pixiv.net',path:'/',secure:true};
 let resolveGrant,resolveRevoke;const timers=[];
 const chrome={tabs:{query:async()=>[{id:1,url:'https://www.pixiv.net/en/'}]},scripting:{executeScript:async({func})=>[{result:func()}]},cookies:{getAll:async()=>{events.push('read');return [cookie,{name:'SID',value:'unrelated',domain:'.google.com',path:'/',secure:true},...(duplicate?[cookie]:[])]}},permissions:{request:async value=>{events.push({grant:value});return pendingGrant?new Promise(resolve=>{resolveGrant=resolve}):grant},remove:async value=>{events.push({remove:value});return pendingRevoke?new Promise(resolve=>{resolveRevoke=resolve}):true}}};
 const fetch=async(url,options={})=>{
  assert.ok(url.startsWith('http://127.0.0.1:12345/'+'a'.repeat(48)+'/'));assert.equal(options.credentials,'omit');assert.equal(options.redirect,'error');
  if(url.endsWith('/request'))return{ok:!requestError,json:async()=>({version:1,platform,account:'alice',destination:'synthetic-vps',expires_in_seconds:expires})};
  assert.ok(url.endsWith('/complete'));events.push({post:JSON.parse(options.body)});return{ok:!completeError,json:async()=>({submitted:true})};
 };
 const context=vm.createContext({window:{addEventListener:()=>{}},document:doc,navigator:{language:'en'},location:{hash:'#'+hash},chrome,fetch,AbortSignal,setTimeout:(fn,ms)=>{const timer={fn,ms,cleared:false};timers.push(timer);return timer},clearTimeout:timer=>{if(timer)timer.cleared=true}});vm.runInContext(source,context);
 await settle();
 if(click&&!nodes.connect.disabled){nodes.connect.click();await settle()}
 return{events,nodes,timers,state:()=>doc.documentElement.dataset.state,resolveGrant:value=>resolveGrant(value),resolveRevoke:value=>resolveRevoke(value)};
}
(async()=>{
 const success=await scenario();const post=success.events.find(v=>v.post)?.post;assert.ok(post);assert.equal(post.cookies.length,1);assert.equal(post.cookies[0].name,'PHPSESSID');assert.equal(post.ready,true);assert.ok(success.events.some(v=>v.remove));assert.match(success.nodes.status.textContent,/verification result/);
 const modern=await scenario({next:true});assert.ok(modern.events.some(v=>v.post),'authenticated Next.js bootstrap can hand off the selected session');
 assert.equal(success.state(),'submitted');assert.equal(success.nodes.connect.disabled,true);assert.match(success.nodes.connect.textContent,/handed off/);assert.ok(success.timers[0].cleared,'successful handoff must not later turn into an expiry error');
 const busy=await scenario({pendingGrant:true});assert.equal(busy.nodes.connect.disabled,true);assert.match(busy.nodes.connect.textContent,/Waiting for browser permission/);busy.nodes.connect.click();assert.equal(busy.events.filter(e=>e.grant).length,1,'repeated clicks must not create additional permission prompts');busy.timers[0].fn();busy.resolveGrant(true);await settle();assert.equal(busy.state(),'expired');assert.ok(!busy.events.includes('read'),'grant after the pairing deadline must not read cookies');
 const expired=await scenario({click:false});expired.timers[0].fn();assert.equal(expired.nodes.connect.disabled,true);assert.match(expired.nodes.connect.textContent,/expired/);expired.nodes.connect.click();assert.equal(expired.events.length,0,'an expired page cannot request permission');
 const revoke=await scenario({ready:false,pendingRevoke:true});assert.equal(revoke.nodes.connect.disabled,true,'retry must wait until old site access has been revoked');revoke.resolveRevoke(true);await settle();assert.equal(revoke.nodes.connect.disabled,false);assert.match(revoke.nodes.connect.textContent,/Try again/);
 for(const options of [{requestError:true},{expires:0},{expires:Infinity},{expires:3601},{hash:'invalid'}]){const r=await scenario(options);assert.equal(r.state(),'expired');assert.equal(r.nodes.connect.disabled,true);assert.match(r.nodes.connect.textContent,/expired/)}
 const failed=await scenario({completeError:true});assert.equal(failed.state(),'expired');assert.equal(failed.nodes.connect.disabled,true);assert.ok(failed.events.some(e=>e.remove));
 for(const options of [{ready:false},{ready:false,next:true},{grant:false},{duplicate:true},{platform:'discord'},{hash:'70000/'+'a'.repeat(48)},{hash:'12345/not-a-pairing-capability'}]){
  const r=await scenario(options);assert.ok(!r.events.some(v=>v.post),'invalid or incomplete contexts must never transfer credentials');
  if(options.grant===false||options.ready===false)assert.ok(!r.events.includes('read'),'rejected or anonymous page must not read cookies');
 }
 console.log(JSON.stringify({selected_site_only:true,positive_readiness_required:true,permission_required_and_revoked:true,unrelated_cookies_filtered:true,malformed_pairing_rejected:true,explicit_button_states:true,deadline_prevents_late_read:true,retry_waits_for_revocation:true,real_account:false}));
})().catch(error=>{console.error(error.message);process.exitCode=1});
