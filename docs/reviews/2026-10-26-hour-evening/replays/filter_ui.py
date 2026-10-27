from ui import *
import fcntl
S='.page:not([hidden])'
def settled(timeout=60):
 wait('return !document.querySelector(".page:not([hidden]) .status")?.innerText.includes("Comparing")',timeout);time.sleep(.08)
def pattern(text,enter=True):
 control('.patterns input',text)
 js('document.querySelector(".page:not([hidden]) .patterns input").focus()')
 if enter:keys('\ue007')
 else:time.sleep(.6)
 settled();return state()
def state():return js('const p=document.querySelector(".page:not([hidden])");return {pattern:p.querySelector(".patterns input")?.value,error:p.querySelector(".c-err, .banner.err")?.innerText??"",status:p.querySelector(".status")?.innerText,filters:[...p.querySelectorAll(".filters button")].map(e=>({id:e.dataset.filter,label:e.innerText})),rows:[...p.querySelectorAll(".viewport .row")].map(e=>({left:e.querySelector("[data-side=left] .name")?.innerText.trim()??"",right:e.querySelector("[data-side=right] .name")?.innerText.trim()??"",status:e.className}))}')
def summary(label):event('filter-state',{'label':label,**state()})
def manual():return (ROOT/'gui.lock').open('w')
