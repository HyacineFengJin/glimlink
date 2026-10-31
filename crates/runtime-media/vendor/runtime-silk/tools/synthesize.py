# SPDX-License-Identifier: MIT
import pathlib,math,struct,json,hashlib,sys
root=pathlib.Path(sys.argv[1]);root.mkdir(parents=True,exist_ok=True)
cases=[]
for name,rate,packet,tencent,pattern in [('silence8',8000,20,False,'silence'),('sine8',8000,20,True,'sine'),('mix12',12000,40,False,'mix'),('mix16',16000,20,True,'mix'),('chirp24',24000,40,False,'chirp'),('noise16',16000,60,True,'noise'),('impulse24',24000,100,False,'impulse'),('dtx8',8000,20,True,'silence'),('dtx16',16000,40,False,'bursts')]:
 samples=[];seed=17
 for i in range(rate*(4 if name.startswith('dtx') else 2)):
  t=i/rate;seed=(seed*1664525+1013904223)&0xffffffff;noise=((seed>>16)-32768)/32768
  if pattern=='silence':v=0
  elif pattern=='sine':v=0.45*math.sin(2*math.pi*440*t)
  elif pattern=='mix':v=(0.32*math.sin(2*math.pi*130*t)+0.12*math.sin(2*math.pi*390*t)+0.05*noise)*(0.25+0.75*abs(math.sin(2*math.pi*2.3*t)))
  elif pattern=='chirp':v=0.45*math.sin(2*math.pi*(90*t+900*t*t))
  elif pattern=='bursts':v=0.5*math.sin(2*math.pi*220*t) if 1.2<=t<1.8 else 0
  elif pattern=='noise':v=noise*0.3 if i%(rate//2)<rate//3 else 0
  else:v=0.8 if i%max(1,rate//130)==0 else (-0.4 if i%max(1,rate//130)==1 else 0)
  samples.append(max(-32768,min(32767,round(v*32767))))
 data=struct.pack('<'+'h'*len(samples),*samples);(root/(name+'.input.pcm')).write_bytes(data)
 cases.append(dict(name=name,rate=rate,packet_ms=packet,tencent=tencent,pattern=pattern,dtx=name.startswith('dtx'),input_sha256=hashlib.sha256(data).hexdigest()))
(root/'cases.json').write_text(json.dumps(cases,indent=2))
