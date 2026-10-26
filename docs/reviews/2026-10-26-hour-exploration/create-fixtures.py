#!/usr/bin/env python3
"""Create disposable fixtures for the 2026-10-26 exploration findings."""
import argparse,json,os,pathlib,subprocess,zipfile
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--directory',type=pathlib.Path,required=True);a=p.parse_args();R=a.directory.expanduser().resolve()
if R.exists()and(not R.is_dir()or any(R.iterdir())):p.error('--directory must be new or empty')
R.mkdir(parents=True,exist_ok=True)
def put(name,text):
 q=R/name;q.parent.mkdir(parents=True,exist_ok=True);q.write_bytes(text.encode()if isinstance(text,str)else text);return str(q)
def git(repo,*args,data=None):return subprocess.run(['git','-C',str(repo),*args],input=data,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout
manifest={}
for name,text in [('editor','alpha\nbeta\n'),('empty','')]:
 manifest[name]={s:put(f'{name}/{s}.txt',text)for s in ['left','right']}
for name,length in [('merge-editor',0),('merge-medium',400),('merge-long',20000)]:
 if length:values={s:'x'*length+' '+s.upper()+'\n'for s in ['base','local','remote']}
 else:values={s:'alpha\nbeta\n'for s in ['base','local','remote']}
 manifest[name]={s:put(f'{name}/{s}.txt',v)for s,v in values.items()};manifest[name]['result']=put(f'{name}/result.txt','initial result\n')
manifest['merge-empty']={s:put(f'merge-empty/{s}.txt','')for s in ['base','local','remote','result']}
for side,label in [('source','NEW'),('dest','OLD')]:
 q=R/'archive'/side/'target.zip';q.parent.mkdir(parents=True,exist_ok=True)
 with zipfile.ZipFile(q,'w',compression=zipfile.ZIP_STORED)as z:
  for entry in ['a.txt','b.txt']:z.writestr(zipfile.ZipInfo(entry,(2020,1,1,0,0,0)),f'{label} {entry[0].upper()}\n')
 os.utime(q,(1700000000,1700000000))
put('archive/reference.txt','OLD A\n')
for side,text in [('left','"<<": {x: 1}\n'),('right','x: 1\n')]:put(f'yaml/{side}/data.yaml',text)
for side,value in [('left','alpha,beta'),('right','beta,alpha')]:
 put(f'needs/{side}/req.rst',f'.. req:: Ordered product code\n   :id: REQ_001\n   :product_code: {value}\n\n   Same content.\n')
 put(f'needs/{side}/conf.py','extensions = ["sphinx_needs"]\nneeds_extra_options = ["product_code"]\nmaster_doc = "req"\nproject = "Custom field fixture"\n')
for side in ['left','right']:
 repo=R/'permissions'/side;repo.mkdir(parents=True);git(repo,'init','-q','-b','main');put(f'permissions/{side}/visible.txt','same\n');put(f'permissions/{side}/restricted/secret.txt',side+' SECRET\n');git(repo,'add','.')
# Permissions are left readable; the README shows the chmod / restore commands.
repo=R/'conflict-many';repo.mkdir();git(repo,'init','-q','-b','main');oids=[git(repo,'hash-object','-w','--stdin',data=(s+' value\n').encode()).decode().strip()for s in ['base','local','remote']]
records=''.join(f'100644 {oid} {s}\tfile{i:04}.txt\n'for i in range(120)for s,oid in enumerate(oids,1));git(repo,'update-index','--index-info',data=records.encode())
repo=R/'git-refs';repo.mkdir();git(repo,'init','-q','-b','main')
blob=git(repo,'hash-object','-w','--stdin',data=b'reference fixture\n').decode().strip()
tree=git(repo,'mktree',data=f'100644 blob {blob}\texample.txt\n'.encode()).decode().strip()
for ref in ['short','feature/a-very-long-reference-name-for-testing-side-specific-dropdown-and-the-path-layout']:git(repo,'update-ref','refs/tags/'+ref,tree)
put('metadata/left.txt',b'a\nb\n');put('metadata/right.txt',b'a\r\nb\r\n')
(R/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n');print(R)
