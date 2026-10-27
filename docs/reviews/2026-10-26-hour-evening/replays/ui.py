import urllib.request,json,pathlib,time,base64,datetime,ctypes,ast
import os
ROOT=pathlib.Path(os.environ.get('PARALLAX_REVIEW_LOGS',str(pathlib.Path(__file__).resolve().parent/'artifacts')))
ROOT.mkdir(parents=True,exist_ok=True)
(ROOT/'screenshots').mkdir(exist_ok=True)
C=pathlib.Path(os.environ.get('PARALLAX_REVIEW_FIXTURES','/tmp/parallax-review-fixtures'))
CASES=json.loads((C/'merge.json').read_text()) if (C/'merge.json').exists() else {}
HTTP=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def request(method,path,body=None,timeout=120):
    data=None if body is None else json.dumps(body).encode()
    req=urllib.request.Request(os.environ.get('PARALLAX_REVIEW_ENDPOINT','http://127.0.0.1:4510')+path,data=data,method=method,headers={'Content-Type':'application/json'})
    try:
        with HTTP.open(req,timeout=timeout) as r:d=json.load(r)
    except urllib.error.HTTPError as e:raise RuntimeError(e.read().decode())
    v=d.get('value')
    if isinstance(v,dict) and 'error' in v and 'message' in v:raise RuntimeError(v)
    return v
def sid():return (ROOT/'session.txt').read_text().strip()
def js(script,*args):return request('POST',f'/session/{sid()}/execute/sync',{'script':script,'args':list(args)})
def asyncjs(script,*args):return request('POST',f'/session/{sid()}/execute/async',{'script':script,'args':list(args)})
def invoke(cmd,**args):return asyncjs('const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(x=>done({ok:true,value:x})).catch(e=>done({ok:false,error:String(e)}));',cmd,args)
def wait(script,timeout=30,*args):
    t=time.monotonic()
    while time.monotonic()-t<timeout:
        v=js(script,*args)
        if v:return v
        time.sleep(.1)
    raise TimeoutError(script)
def visible():return js('return document.querySelector(".page:not([hidden])")?.innerText ?? document.body.innerText')
def click(text,scope='.page:not([hidden])',prefix=False):
    return js('const root=document.querySelector(arguments[1]);const e=[...root.querySelectorAll("button")].find(e=>(arguments[2]?e.innerText.trim().startsWith(arguments[0]):e.innerText.trim()===arguments[0])&&e.getClientRects().length);if(!e)throw Error("Missing "+arguments[0]);e.focus();e.click();return true',text,scope,prefix)
def input(label,value):return js('const e=[...document.querySelectorAll("input")].find(e=>e.getAttribute("aria-label")===arguments[0]&&e.getClientRects().length);if(!e)throw Error("Missing "+arguments[0]);e.focus();e.value=arguments[1];e.dispatchEvent(new Event("input",{bubbles:true}));return true',label,str(value))
def new():
    js('document.querySelector(".tabs .add").click()')
    wait('return !!document.querySelector(".page:not([hidden]) .dialog")')
def merge(name,**overrides):
    case={**CASES[name],**overrides};new();click('Merge three files…')
    for label,key in [('Local','local'),('Base','base'),('Remote','remote'),('Result','output')]:input(label,case.get(key,''))
    t=time.monotonic();click('Merge →')
    wait('return !!document.querySelector(".page:not([hidden]) .merge")')
    wait('return !document.querySelector(".page:not([hidden]) .merge").innerText.includes("Merging…")')
    wait('return !document.querySelector(".page:not([hidden]) .rrow.loading")')
    event('open-merge',{'case':name,'wallSeconds':time.monotonic()-t,'text':visible()[:1600]});return case
def conflicts(repo):
    new();click('Resolve conflicts in a repository…');input('Repository',repo);click('Show conflicts →')
    wait('return !!document.querySelector(".page:not([hidden]) .conflicts table")||!!document.querySelector(".page:not([hidden]) .conflicts .empty")')
    event('open-conflicts',{'repo':str(repo),'text':visible()[:3000]})
def keys(*keys):return request('POST',f'/session/{sid()}/actions',{'actions':[{'type':'key','id':'merge-keyboard','actions':[a for k in keys for a in [{'type':'keyDown','value':k},{'type':'keyUp','value':k}]]}]})
def combo(*keys):return request('POST',f'/session/{sid()}/actions',{'actions':[{'type':'key','id':'merge-keyboard','actions':[{'type':'keyDown','value':k}for k in keys]+[{'type':'keyUp','value':k}for k in reversed(keys)]}]})
def event(name,data):
    with (ROOT/'runtime.jsonl').open('a') as f:f.write(json.dumps({'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'elapsed':elapsed(),'name':name,'data':data})+'\n')
    print(name,json.dumps(data)[:1600],flush=True)
def elapsed():return time.monotonic()-json.loads((ROOT/'started.json').read_text())['monotonic'] if (ROOT/'started.json').exists() else 0
def snapshot(name):
    data=request('GET',f'/session/{sid()}/screenshot');(ROOT/'screenshots'/f'{name}.png').write_bytes(base64.b64decode(data));(ROOT/f'{name}.txt').write_text(visible());event('screenshot',name)
def finding(id,severity,title,steps,actual,expected,fix,evidence):
    p=ROOT/'findings.json';items=json.loads(p.read_text())if p.exists()else[]
    items=[f for f in items if f['id']!=id]+[dict(id=id,severity=severity,title=title,steps=steps,actual=actual,expected=expected,fix=fix,evidence=evidence)]
    p.write_text(json.dumps(items,indent=2)+'\n');event('finding',items[-1])
def desktop_key(name):
    x=ctypes.CDLL('libX11.so.6');t=ctypes.CDLL('libXtst.so.6');x.XOpenDisplay.argtypes=[ctypes.c_char_p];x.XOpenDisplay.restype=ctypes.c_void_p;display=x.XOpenDisplay(b':99')
    x.XStringToKeysym.argtypes=[ctypes.c_char_p];x.XStringToKeysym.restype=ctypes.c_ulong;x.XKeysymToKeycode.argtypes=[ctypes.c_void_p,ctypes.c_ulong];x.XKeysymToKeycode.restype=ctypes.c_uint
    t.XTestFakeKeyEvent.argtypes=[ctypes.c_void_p,ctypes.c_uint,ctypes.c_int,ctypes.c_ulong];x.XFlush.argtypes=[ctypes.c_void_p];x.XCloseDisplay.argtypes=[ctypes.c_void_p]
    code=x.XKeysymToKeycode(display,x.XStringToKeysym(name.encode()));t.XTestFakeKeyEvent(display,code,1,0);t.XTestFakeKeyEvent(display,code,0,0);x.XFlush(display);x.XCloseDisplay(display)
def start():
    r=request('POST','/session',{'capabilities':{'alwaysMatch':{'tauri:options':{'application':os.environ['PARALLAX_REVIEW_BINARY']}}}})
    (ROOT/'session.txt').write_text(r['sessionId']);(ROOT/'capabilities.json').write_text(json.dumps(r,indent=2));(ROOT/'started.json').write_text(json.dumps({'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'monotonic':time.monotonic()}))
    request('POST',f'/session/{sid()}/timeouts',{'script':120000});request('POST',f'/session/{sid()}/window/rect',{'width':1400,'height':900})
    js('window.exploreErrors=[];window.addEventListener("error",e=>window.exploreErrors.push(e.message));window.addEventListener("unhandledrejection",e=>window.exploreErrors.push(String(e.reason)))')
    assert js('return location.href').startswith(('tauri:', 'http://tauri.localhost', 'https://tauri.localhost')), 'Expected bundled UI'
    wait('return !!document.querySelector(\"input[aria-label^=Left]\")');event('started',r);snapshot('00-startup')
if __name__=='__main__':start()
def compare(left,right):
    new();input('Left path',left);input('Right path',right)
    wait('return [...document.querySelectorAll(".page:not([hidden]) button")].some(e=>e.innerText.trim()==="Compare →"&&!e.disabled)',30)
    t=time.monotonic();click('Compare →')
    wait('return !document.querySelector(".page:not([hidden]) .dialog")',120)
    wait('return !document.querySelector(".page:not([hidden]) .row.loading")',120)
    time.sleep(.2);event('open-comparison',{'left':str(left),'right':str(right),'seconds':time.monotonic()-t,'text':visible()[:2000]})
def close_active():
    js('document.querySelector(".tabs .tab.on .x").click()');time.sleep(.15)
def control(selector,value):
    return js('const e=document.querySelector(".page:not([hidden]) "+arguments[0]);if(!e)throw Error("Missing control "+arguments[0]);e.value=arguments[1];e.dispatchEvent(new Event("input",{bubbles:true}));e.dispatchEvent(new Event("change",{bubbles:true}));return true',selector,value)
def check(name,condition,detail=None):
    event('check',{'name':name,'passed':bool(condition),'detail':detail})
    return bool(condition)
def run(name,fn):
    import traceback
    try: fn()
    except Exception as e:
        event('harness-error',{'case':name,'error':str(e),'traceback':traceback.format_exc()})
        try:snapshot('error-'+name)
        except:pass
