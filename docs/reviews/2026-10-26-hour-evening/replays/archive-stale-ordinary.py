from manual import *
from PIL import ImageGrab
import zipfile,os
A=C/'archive-stale';A.mkdir(exist_ok=True);(A/'left.txt').write_text('old A\n')
def pack(path,a,b):
 with zipfile.ZipFile(path,'w')as z:z.writestr('a.txt',a);z.writestr('b.txt',b)
def replace(path,a,b):
 stamp=path.stat();tmp=path.with_suffix('.new.zip');pack(tmp,a,b);os.replace(tmp,path);None # ordinary replacement keeps the new mtime
def read(path):
 with zipfile.ZipFile(path)as z:return {n:z.read(n).decode()for n in z.namelist()}
with manual():
 p=A/'dirty.zip';pack(p,'old A\n','old B\n');compare(A/'left.txt',str(p)+'!/a.txt');settled()
 js('document.querySelector(".page:not([hidden]) .viewport .row .code[data-side=right]").dispatchEvent(new MouseEvent("click",{bubbles:true}))');wait('return !!document.querySelector(".page:not([hidden]) input.edit")');control('input.edit','my edited A');js('document.activeElement.blur()');time.sleep(.2)
 replace(p,'EXT A\n','EXT B\n');time.sleep(1.1);event('archive-external-before-save',{'archive':read(p),'text':visible()});snapshot('50-archive-external-before-save')
 combo('\ue009','s');time.sleep(.5);ImageGrab.grab(xdisplay=':99').save(ROOT/'screenshots/51-archive-save-native.png');after=read(p);event('archive-external-after-save',{'archive':after,'text':visible()});snapshot('52-archive-after-stale-save')
 desktop_key('Escape')
 if after['a.txt']=='EXT A\n':combo('\ue009','z');time.sleep(.2)
 close_active()
 p=A/'clean.zip';pack(p,'old A\n','old B\n');compare(A/'left.txt',str(p)+'!/a.txt');settled();replace(p,'EXT A\n','EXT B\n');time.sleep(1.1);event('archive-external-auto-reload',{'archive':read(p),'text':visible()});snapshot('53-archive-clean-stale-view');close_active()
