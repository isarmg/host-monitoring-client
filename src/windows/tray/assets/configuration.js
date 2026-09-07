(async()=>{
const app=document.getElementById('app');
const output=document.getElementById('result');
const connection=document.getElementById('connection');
const service=document.getElementById('service');
const serverInput=document.getElementById('server');
const actionButtons=[...document.querySelectorAll('button[data-service],#pair-submit')];
const headers=token=>({Authorization:'Bearer '+token,'Content-Type':'application/json','X-Host-Monitoring-Tray':'1'});
let bearer=sessionStorage.getItem('hostMonitoringTrayBearer')||'';
let operationPending=false;
let serviceCode='unknown';
let initialized=false;
let lastState={};
let lastConnection={status:'unknown',message:'尚未检测'};
let connectionPending=false;
let connectionGeneration=0;
let connectionTimer=0;
const rawHash=location.hash.slice(1);
let bootstrap='';
let target=rawHash==='pair'?'pair':'status';
const capability=rawHash.match(/^([0-9a-f]{64})(?::(pair|status))?$/);
if(capability){bootstrap=capability[1];target=capability[2]||'status'}
history.replaceState(null,'',target==='pair'?'#pair':'/');

function setResult(message,kind='info'){
  output.textContent=message;
  output.dataset.kind=kind;
  output.setAttribute('role',kind==='error'?'alert':'status');
}
function updateButtons(){
  document.getElementById('pair-submit').disabled=operationPending;
  document.querySelector('[data-service=start]').disabled=operationPending||serviceCode==='running'||serviceCode==='starting';
  document.querySelector('[data-service=stop]').disabled=operationPending||serviceCode==='stopped'||serviceCode==='stopping';
}
function setBusy(value){operationPending=value;app.setAttribute('aria-busy',String(value));updateButtons()}
async function api(path,body){
  const response=await fetch(path,{method:'POST',headers:headers(bearer),body:JSON.stringify(body)});
  const result=await response.json().catch(()=>({code:'invalid_response',message:'本地控制服务返回了无法解析的响应'}));
  if(!response.ok){
if(response.status===401){sessionStorage.removeItem('hostMonitoringTrayBearer')}
const error=new Error(result.message||('HTTP '+response.status));
error.code=result.code||'request_failed';
throw error;
  }
  return result;
}
async function refreshState(populate=false){
  const state=await api('/state',{});
  lastState=state;
  service.textContent=state.service;
  serviceCode=state.service_code||'unknown';
  document.getElementById('version').textContent=state.version?'v'+state.version:'';
  if(populate&&!initialized){serverInput.value=state.server||'';initialized=true}
  updateButtons();
}
try{
  if(bootstrap){
const response=await fetch('/session',{method:'POST',headers:headers(bootstrap),body:'{}'});
const result=await response.json().catch(()=>({message:'本地会话交换失败'}));
if(!response.ok)throw new Error(result.message||('HTTP '+response.status));
bearer=result.bearer;
sessionStorage.setItem('hostMonitoringTrayBearer',bearer);
  }
  if(!/^[0-9a-f]{64}$/.test(bearer))throw new Error('请从托盘菜单重新打开配置');
  await refreshState(true);
  app.setAttribute('aria-busy','false');
}catch(error){
  sessionStorage.removeItem('hostMonitoringTrayBearer');
  setResult('无法建立本地安全会话：'+error.message,'error');
  app.setAttribute('aria-busy','false');
  actionButtons.forEach(button=>{button.disabled=true});
  return;
}

async function checkConnection(){
  if(connectionPending||document.hidden)return;
  connectionPending=true;
  const generation=++connectionGeneration;
  const server=String(serverInput.value||'');
  connection.textContent='正在检测…';
  try{
const result=await api('/connection',{server});
lastConnection=result;
if(generation===connectionGeneration&&server===String(serverInput.value||'')){
  connection.textContent=result.message;
  connection.dataset.status=result.status||'offline';
}
  }catch(error){
if(generation===connectionGeneration){connection.textContent='检测失败：'+error.message;connection.dataset.status='offline'}
  }finally{connectionPending=false}
}
function scheduleConnectionCheck(delay=30000){
  clearTimeout(connectionTimer);
  connectionTimer=setTimeout(()=>{if(!document.hidden)void checkConnection();scheduleConnectionCheck()},delay);
}
async function followOperation(id){
  const deadline=Date.now()+20*60*1000;
  while(Date.now()<deadline){
const operation=await api('/operation',{id});
setResult(operation.message,operation.terminal?(operation.success?'success':'error'):'info');
if(operation.terminal){
  setBusy(false);
  await refreshState(false);
  void checkConnection();
  return;
}
await new Promise(resolve=>setTimeout(resolve,1000));
  }
  setBusy(false);
  throw new Error('操作状态等待超时；后台操作可能仍在继续，请稍后刷新状态');
}
async function startOperation(path,body){
  if(operationPending)return;
  setBusy(true);
  setResult('正在请求 Windows 权限确认…');
  try{
const result=await api(path,body);
setResult(result.message||'操作已开始');
if(!result.operation_id)throw new Error('本地控制服务没有返回操作编号');
await followOperation(result.operation_id);
  }catch(error){setBusy(false);setResult('操作失败：'+error.message,'error')}
}
document.getElementById('check-connection').addEventListener('click',()=>{void checkConnection()});
serverInput.addEventListener('input',()=>{connectionGeneration++;clearTimeout(connectionTimer);connectionTimer=setTimeout(()=>{void checkConnection();scheduleConnectionCheck()},700)});
document.addEventListener('visibilitychange',()=>{if(!document.hidden){void refreshState(false);void checkConnection()} });
document.querySelector('form[data-endpoint]').addEventListener('submit',event=>{
  event.preventDefault();
  const form=new FormData(event.currentTarget);
  const codeInput=event.currentTarget.elements.activation_code;
  const activationCode=String(form.get('activation_code')||'');
  codeInput.value='';
  void startOperation('/pair',{server:String(form.get('server')||''),activation_code:activationCode});
});
document.querySelectorAll('[data-service]').forEach(button=>button.addEventListener('click',()=>{
  const action=button.dataset.service;
  if(action==='stop'&&!confirm('停止 Client 服务只影响本次开机；下次启动 Windows 时仍会自动运行。继续吗？'))return;
  void startOperation('/service',{action});
}));
document.getElementById('copy-diagnostics').addEventListener('click',async()=>{
  const summary=[
'host-monitor tray '+(lastState.version||'unknown'),
'Service: '+(lastState.service_code||'unknown')+' ('+(lastState.service||'unknown')+')',
'Management origin: '+(serverInput.value||'not configured'),
'Reachability: '+(lastConnection.status||'unknown')+' ('+(lastConnection.message||'')+')',
'Platform: Windows '+navigator.userAgent
  ].join('\n');
  try{await navigator.clipboard.writeText(summary);setResult('脱敏诊断已复制到剪贴板','success')}
  catch(_){setResult('浏览器未允许访问剪贴板，请手动复制页面中的状态信息','error')}
});
if(target==='pair'){document.getElementById('pair').scrollIntoView();serverInput.focus()}
void checkConnection();
scheduleConnectionCheck();
setInterval(()=>{if(!document.hidden&&!operationPending)void refreshState(false)},10000);
})();
