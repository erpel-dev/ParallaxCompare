from pathlib import Path
import urllib.request,json,time,re
ROOT=Path('/workspace/merge-exploration-20261025');C=Path('/tmp/parallax-merge-exploration-20261025');HTTP=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def request(method,path,body=None):
    data=None if body is None else json.dumps(body).encode();r=HTTP.open(urllib.request.Request('http://127.0.0.1:4452'+path,data=data,method=method,headers={'Content-Type':'application/json'}),timeout=120);return json.load(r).get('value')
r=request('POST','/session',{'capabilities':{'alwaysMatch':{'tauri:options':{'application':'/tmp/parallax-exploration-target/release/parallax-compare'}}}});sid=r['sessionId'];pid=r['capabilities']['webkit:processID'];(ROOT/'memory-session.json').write_text(json.dumps(r));request('POST',f'/session/{sid}/timeouts',{'script':120000})
def invoke(cmd,**args):
    return request('POST',f'/session/{sid}/execute/async',{'script':'const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(v=>done({ok:true,value:v})).catch(e=>done({ok:false,error:String(e)}));','args':[cmd,args]})
def memory():
    s=Path(f'/proc/{pid}/status').read_text();return {k:int(re.search(r'^'+k+r':\s+(\d+)',s,re.M)[1])for k in ['VmRSS','VmHWM','RssAnon','RssFile']}
results=[];time.sleep(1)
for case in ['baseline','million-open','million-syntax','million-closed','million-reopened','million-reclosed']:
    t=time.monotonic()
    if case in ['million-open','million-reopened']:
        d=C/'million';req={'kind':'sources',**{s:{'type':'local','path':str(d/(s+'.txt'))}for s in ['base','local','remote']},'output':str(d/'fresh-result.txt')};opened=invoke('open_merge',request=req);assert opened['ok'];id=opened['value']['id']
    if case=='million-syntax':assert invoke('merge_structured',id=id,engine='builtin',mergiraf=None)['ok']
    if case in ['million-closed','million-reclosed']:assert invoke('close_merge',id=id)['ok'];time.sleep(1)
    r={'case':case,'seconds':time.monotonic()-t,'memoryKiB':memory()};results.append(r);print(json.dumps(r),flush=True);(ROOT/'fresh-memory-results.json').write_text(json.dumps(results,indent=2)+'\n')
request('DELETE',f'/session/{sid}')
