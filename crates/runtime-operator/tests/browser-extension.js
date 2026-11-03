// SPDX-License-Identifier: MIT
// Synthetic browser APIs only. No browser profile, account or network is used.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const source=fs.readFileSync(require('node:path').join(__dirname,'../src/browser_login/extension/connect.js'),'utf8');
async function scenario({ready=true,grant=true,hash='12345/'+'a'.repeat(48),platform='pixiv',duplicate=false}={}){
 const events=[],nodes=Object.fromEntries(['connect','status','heading','target','scope'].map(id=>[id,{disabled:true,textContent:'',addEventListener:(_,f)=>nodes[id].click=f}]));
 const doc={getElementById:id=>nodes[id],querySelector:()=>({content:JSON.stringify({userData:ready?{id:'42',name:'synthetic'}:null})})};
 const cookie={name:'PHPSESSID',value:'synthetic-cookie',domain:'.pixiv.net',path:'/',secure:true};
 const chrome={tabs:{query:async()=>[{id:1,url:'https://www.pixiv.net/en/'}]},scripting:{executeScript:async({func})=>[{result:func()}]},cookies:{getAll:async()=>{events.push('read');return [cookie,{name:'SID',value:'unrelated',domain:'.google.com',path:'/',secure:true},...(duplicate?[cookie]:[])]}},permissions:{request:async value=>{events.push({grant:value});return grant},remove:async value=>{events.push({remove:value});return true}}};
 const fetch=async(url,options={})=>{
  assert.ok(url.startsWith('http://127.0.0.1:12345/'+'a'.repeat(48)+'/'));assert.equal(options.credentials,'omit');assert.equal(options.redirect,'error');
  if(url.endsWith('/request'))return{ok:true,json:async()=>({version:1,platform,account:'alice',destination:'synthetic-vps',expires_in_seconds:60})};
  assert.ok(url.endsWith('/complete'));events.push({post:JSON.parse(options.body)});return{ok:true,json:async()=>({submitted:true})};
 };
 const context=vm.createContext({window:{addEventListener:()=>{}},document:doc,navigator:{language:'en'},location:{hash:'#'+hash},chrome,fetch,AbortSignal});vm.runInContext(source,context);
 const tick=()=>new Promise(r=>setImmediate(r));for(let i=0;i<6;i++)await tick();
 if(!nodes.connect.disabled){nodes.connect.click();for(let i=0;i<12;i++)await tick();}
 return{events,nodes};
}
(async()=>{
 const success=await scenario();const post=success.events.find(v=>v.post)?.post;assert.ok(post);assert.equal(post.cookies.length,1);assert.equal(post.cookies[0].name,'PHPSESSID');assert.equal(post.ready,true);assert.ok(success.events.some(v=>v.remove));assert.match(success.nodes.status.textContent,/verification result/);
 for(const options of [{ready:false},{grant:false},{duplicate:true},{platform:'discord'},{hash:'70000/'+'a'.repeat(48)},{hash:'12345/not-a-pairing-capability'}]){
  const r=await scenario(options);assert.ok(!r.events.some(v=>v.post),'invalid or incomplete contexts must never transfer credentials');
  if(options.grant===false||options.ready===false)assert.ok(!r.events.includes('read'),'rejected or anonymous page must not read cookies');
 }
 console.log(JSON.stringify({selected_site_only:true,positive_readiness_required:true,permission_required_and_revoked:true,unrelated_cookies_filtered:true,malformed_pairing_rejected:true,real_account:false}));
})().catch(error=>{console.error(error.message);process.exitCode=1});
