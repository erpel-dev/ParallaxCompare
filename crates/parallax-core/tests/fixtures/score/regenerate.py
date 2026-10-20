#!/usr/bin/env python3
"""Invented comparison fixtures. Regeneration needs flatc 24.3.25 (or compatible)."""
from pathlib import Path
import json, subprocess, tempfile
ROOT = Path(__file__).parent

def write(name, text):
    (ROOT/name).write_text(text, encoding='utf-8')

def configs(name, value, changed):
    write(name+'-v1.json', json.dumps(value, indent=2, ensure_ascii=False)+'\n')
    def reorder(v):
        if isinstance(v,dict): return {k:reorder(v[k]) for k in reversed(v)}
        return [reorder(x) for x in v] if isinstance(v,list) else v
    formatted = reorder(value)
    if name=='communication':
        formatted['serviceTypes'].reverse();formatted['serviceInstances'].reverse()
    if name=='logging': formatted['contextConfigs'].reverse()
    if name=='launch': formatted['components'].reverse();formatted['run_targets'].reverse()
    write(name+'-reordered.json', json.dumps(formatted, separators=(',',':'),ensure_ascii=False)+'\n')
    write(name+'-v2.json', json.dumps(changed, indent=2,ensure_ascii=False)+'\n')

v={'serviceTypes':[{'serviceTypeName':'Engine','version':{'major':1,'minor':0},'bindings':[{'binding':'SHM','serviceId':12,'events':[{'eventName':'Speed','eventId':2},{'eventName':'Torque','eventId':3}]}]},{'serviceTypeName':'Brakes','version':{'major':1,'minor':0},'bindings':[]}], 'serviceInstances':[{'instanceSpecifier':'vehicle/engine','serviceTypeName':'Engine','version':{'major':1,'minor':0},'instances':[{'binding':'SHM','instanceId':22,'numberOfSampleSlots':16}]},{'instanceSpecifier':'vehicle/brakes','instances':[]}], 'global':{'applicationID':42}, 'vendor':{'ordered':[1,2,3]}}
c=json.loads(json.dumps(v));c['serviceInstances'][0]['instances'][0]['numberOfSampleSlots']=32;configs('communication',v,c)
v={'schema_version':1,'components':[{'name':'Engine','process_arguments':['--mode','safe']},{'name':'Logger','process_arguments':['--verbose']}],'run_targets':[{'name':'Drive','depends_on':['Engine','Logger']},{'name':'Park','depends_on':['Logger']}],'initial_run_target':'Park'}
c=json.loads(json.dumps(v));c['components'][0]['process_arguments']=['--mode','sport'];configs('launch',v,c)
v={'appId':'ECU','logLevel':'kInfo','contextConfigs':[{'name':'Engine','logLevel':'kWarn'},{'name':'Brakes','logLevel':'kError'}]};c=json.loads(json.dumps(v));c['contextConfigs'][0]['logLevel']='kDebug';configs('logging',v,c)
v={'parameterSets':{'Engine':{'parameters':{'Idle':{'initValue':800},'Map':{'initValue':[[1,2],[3,4]]}}}}};c=json.loads(json.dumps(v));c['parameterSets']['Engine']['parameters']['Idle']['initValue']=850;configs('parameters',v,c)

trlc='''package Engine
import ScoreReq
ScoreReq.FeatReq SpeedLimit {
 description = "Speed must stay below the configured limit."
 safety = ScoreReq.Asil.B
 version = 1
 derived_from = [System.Limit@1, System.Driver@2]
}
ScoreReq.FeatReq UnicodeText {
 description = "Grüße 🚙"
 version = 2
}
'''
write('requirements-v1.trlc',trlc)
write('requirements-reordered.trlc','package Engine\nimport ScoreReq\n'+trlc[trlc.index('ScoreReq.FeatReq UnicodeText'):]+trlc[trlc.index('ScoreReq.FeatReq SpeedLimit'):trlc.index('ScoreReq.FeatReq UnicodeText')])
write('requirements-v2.trlc',trlc.replace('System.Limit@1','System.Limit@2'))
formats={
 'rsl': 'package ScoreReq\nabstract type Requirement {\n description String\n version Integer\n}\ntuple ReqId {\n item Requirement\n separator @\n version Integer\n}\n',
 'proto': 'syntax = "proto3";\npackage vehicle;\nmessage Speed { uint64 value = 1; repeated string sources = 2; }\nservice Engine { rpc Read (Speed) returns (Speed); }\n',
 'fidl': 'package vehicle\ntypeCollection Types { struct Speed { UInt64 value String unit } }\ninterface Engine { version { major 1 minor 0 } method Read { in { UInt32 channel String unit } out { Types.Speed speed } } }\n',
 'fdepl': 'package vehicle\ndefine vehicle.Engine for provider EngineProvider { instance Engine { InstanceId = 42 SomeIpServiceID = 1234 } }\n',
}
for ext,text in formats.items():
    write('definitions-v1.'+ext,text)
    if ext=='rsl': formatted=text.replace(' description String','   description    String').replace(' version Integer','   version    Integer')
    elif ext in ['fidl','fdepl']: formatted=text.replace('{','{\n ').replace('}','\n}')
    else: formatted=text.replace(';',';\n').replace('{','{\n ').replace('}','\n}')
    write('definitions-formatted.'+ext,'\n'.join(line.rstrip() for line in formatted.splitlines())+'\n')
    changed=text.replace('version Integer','version String') if ext=='rsl' else text.replace('value = 1','value = 3') if ext=='proto' else text.replace('UInt64 value','Int64 value') if ext=='fidl' else text.replace('1234','1235')
    write('definitions-v2.'+ext,changed)

write('types/common.fbs','namespace Demo;\nstruct Point { coords:[float:2]; }\nenum Mode:ubyte { OFF=0, ON=1 }\ntable Option { label:string; }\nunion Choice { Option, Text:string, Location:Point }\n')
fbs='''include "types/common.fbs";
namespace Demo;
table Config {
 /// @required
 name:string (required);
 speed:double = 10.5;
 count:ulong = 9007199254740993;
 mode:Mode = ON;
 position:Point;
 samples:[short];
 points:[Point];
 choice:Choice;
 choices:[Choice];
 optional_count:int = null;
 enabled:bool = true;
 greeting:string;
}
root_type Config;
file_identifier "SCFG";
'''
write('config.fbs',fbs)
write('config-formatted.fbs',fbs.replace(':', ' : ').replace(' = ', '='))
write('config-v2.fbs',fbs.replace('speed:double = 10.5','speed:double = 11.5'))
v={'name':'ECU','position':{'coords':[1.25,-0.0]},'samples':[-32768,0,32767],'points':[{'coords':[3.5,4.5]}],'choice_type':'Option','choice':{'label':'safe'},'choices_type':['Text','Location'],'choices':['hello',{'coords':[9.0,10.0]}],'greeting':'Grüße 🚙'}
with tempfile.TemporaryDirectory() as tmp:
    tmp=Path(tmp)
    def run(*args): subprocess.run(['flatc',*map(str,args)],check=True,capture_output=True)
    run('--binary','--schema','-o',ROOT,ROOT/'config.fbs')
    write('config-expanded.fbs',fbs.replace(' greeting:string;',' greeting:string;\n extra:int = 0;'))
    run('--binary','--schema','-o',ROOT,ROOT/'config-expanded.fbs')
    for name,value,flags,schema in [('data-v1',v,[], 'config.fbs'),('data-repacked',dict(v,speed=10.5,count=9007199254740993,mode='ON',enabled=True),['--force-defaults'],'config.fbs'),('data-size-prefixed',v,['--size-prefixed'],'config.fbs'),('data-v2',dict(v,speed=12.5,count=9007199254740994),[],'config.fbs'),('data-unknown',dict(v,extra=7),[],'config-expanded.fbs')]:
        p=tmp/(name+'.json');p.write_text(json.dumps(value,ensure_ascii=False))
        run('--binary',*flags,'-o',ROOT,ROOT/schema,p)
