from manual import *
with manual():
 for files in [True,False]:
  baseline=js('return [...document.querySelectorAll(".tabs .tab")].map(e=>e.dataset.key)');new()
  input('Left path',C/'cancel-form/left/changed.txt'if files else C/'cancel-form/left');input('Right path',C/'cancel-form/right/changed.txt'if files else C/'cancel-form/right')
  wait('return [...document.querySelectorAll(".page:not([hidden]) button")].some(e=>e.innerText.trim()==="Compare →"&&!e.disabled)')
  formKey=js('return document.querySelector(".tabs .tab.on").dataset.key')
  js('const b=[...document.querySelectorAll(".page:not([hidden]) button")].find(e=>e.innerText.trim()==="Compare →");b.click();document.querySelector(".tabs .tab.on .x").click()')
  time.sleep(.7);event('close-during-open-session',{'files':files,'closedForm':formKey,'baseline':baseline,'tabs':js('return [...document.querySelectorAll(".tabs .tab")].map(e=>({key:e.dataset.key,title:e.innerText,active:e.classList.contains("on")}))'),'text':visible()[:1100]});snapshot('70-cancel-new-form-'+str(files))
  extras=js('return [...document.querySelectorAll(".tabs .tab")].map(e=>e.dataset.key)').copy()
  for key in extras:
   if key not in baseline:js('document.querySelector(`.tabs .tab[data-key="${arguments[0]}"] .x`).click()',key);time.sleep(.1)
