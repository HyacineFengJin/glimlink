// SPDX-License-Identifier: MIT
// No background worker, storage, telemetry, arbitrary request URLs or passwords.
// Platform access is requested by a user click, then revoked after the handoff.
'use strict';
const profiles=Object.freeze({pixiv:{name:'pixiv',roots:['https://www.pixiv.net/','https://pixiv.net/'],cookies:['PHPSESSID']}});
const zh=navigator.language.toLowerCase().startsWith('zh');
const say=(en,cn)=>zh?cn:en;
const button=document.getElementById('connect'),status=document.getElementById('status');
let request=null,base=null,profile=null;
window.addEventListener('pagehide',()=>{if(profile)chrome.permissions.remove({origins:profile.roots.map(root=>root+'*')}).catch(()=>{})});
async function rpc(path,options={}){
  const response=await fetch(base+path,{...options,credentials:'omit',redirect:'error',cache:'no-store',referrerPolicy:'no-referrer',signal:AbortSignal.timeout(10000)});
  if(!response.ok)throw new Error('bridge_unavailable');
  return response.json();
}
function authenticatedPixiv(){
  try{
    const node=document.querySelector('meta[name="global-data"]');
    if(node){const user=JSON.parse(node.content).userData;return Boolean(user&&/^[1-9][0-9]{0,19}$/.test(String(user.id))&&typeof user.name==='string'&&user.name.length)}
    const next=document.getElementById('__NEXT_DATA__');
    const props=next?JSON.parse(next.textContent)?.props?.pageProps:null;
    return Boolean(props?.isLoggedIn===true&&/^[1-9][0-9]{0,19}$/.test(String(props.gaUserData?.userId)));
  }catch{return false}
}
async function readSelectedSession(){
  const tabs=await chrome.tabs.query({url:profile.roots.map(root=>root+'*')});
  let ready=false;
  for(const tab of tabs.slice(0,16)){
    if(!Number.isInteger(tab.id)||!profile.roots.some(root=>tab.url?.startsWith(root)))continue;
    try{const result=await chrome.scripting.executeScript({target:{tabId:tab.id},func:authenticatedPixiv});if(result.some(r=>r.result===true)){ready=true;break}}catch{}
  }
  if(!ready)throw new Error('not_ready');
  const cookies=await chrome.cookies.getAll({url:profile.roots[0]});
  const kept=cookies.filter(c=>profile.cookies.includes(c.name)&&c.path==='/'&&['.pixiv.net','pixiv.net','www.pixiv.net','.www.pixiv.net'].includes(c.domain));
  if(kept.length!==1||!kept[0].value)throw new Error('not_ready');
  return kept.map(({name,value,domain,path,secure})=>({name,value,domain,path,secure}));
}
button.addEventListener('click',()=>{
  if(!request||!profile||button.disabled)return;
  button.disabled=true;
  const origins=profile.roots.map(root=>root+'*');
  // Must remain the first asynchronous action: Chrome requires a user gesture.
  const grant=chrome.permissions.request({origins});
  (async()=>{
    let cookies=null;
    try{
      if(!await grant)throw new Error('permission_denied');
      status.textContent=say('Reading the selected signed-in account…','正在读取选中的已登录账号…');
      cookies=await readSelectedSession();
      const result=await rpc('/complete',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({version:1,platform:request.platform,ready:true,cookies})});
      if(result.submitted!==true)throw new Error('bridge_unavailable');
      request=null;
      status.textContent=say('Session handed to Runtime. Return to Operator for the verification result.','会话已交给 Runtime。请回操控端查看账号核验结果。');
    }catch(error){
      status.textContent=error.message==='not_ready'?say('Keep an already signed-in pixiv homepage open in this browser, then try again. A login or security-check page is not sufficient.','请在这个浏览器保留已登录的 pixiv 首页，再重试。登录页或人机验证页不代表登录完成。'):error.message==='permission_denied'?say('Site access was not granted. Nothing was transferred.','未授予站点访问权限，没有交接会话。'):say('Connection expired or failed. Open a new connection from Operator.','连接已过期或未完成，请从操控端重新连接。');
      button.disabled=false;
    }finally{
      cookies=null;
      try{if(await chrome.permissions.remove({origins})!==true)throw new Error('revoke_failed')}catch{status.textContent+=' '+say('Site access could not be revoked automatically. Remove pixiv access in the extension manager.','未能自动撤销站点权限，请在扩展管理页移除 pixiv 访问权限。')}
    }
  })();
});
(async()=>{
  try{
    const fragment=location.hash.slice(1);
    if(!/^\d{1,5}\/[a-f0-9]{48}$/.test(fragment))return;
    const [port,nonce]=fragment.split('/');
    if(Number(port)<1||Number(port)>65535)return;
    base=`http://127.0.0.1:${port}/${nonce}`;
    const value=await rpc('/request');
    if(value.version!==1||!Object.hasOwn(profiles,value.platform)||typeof value.account!=='string'||value.account.length>128||typeof value.destination!=='string'||value.destination.length>512||typeof value.expires_in_seconds!=='number'||value.expires_in_seconds<=0)throw new Error('invalid_request');
    request=value;profile=profiles[value.platform];
    document.getElementById('heading').textContent=say('Connect ','连接 ')+profile.name;
    document.getElementById('target').textContent=`${value.destination} · ${value.account}`;
    document.getElementById('scope').textContent=say('Only this site’s session goes to your local Operator, then to the selected Runtime. Site access is revoked afterwards.','仅交接这个站点的会话到本机操控端，再发给选中的 Runtime。交接后撤销站点访问权限。');
    button.textContent=say('Connect this account','连接这个账号');button.disabled=false;
    status.textContent=say('Use the account already signed in in this browser. No password is read.','使用当前浏览器已登录的账号，不读取密码。');
  }catch{status.textContent=say('Pairing expired. Open a new connection from Operator.','配对已过期，请从操控端重新打开连接。')}
})();
