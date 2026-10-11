#!/usr/bin/env python3
"""Drive four isolated native PhotoCraft windows over their authenticated controls.
Developer tooling only. Ports must already be reserved in Documents/ports.txt.
"""
import argparse, concurrent.futures, hashlib, json, math, os, socket, struct, subprocess, time, zlib
from pathlib import Path

class Client:
    def __init__(self, port, token):
        self.sock = socket.create_connection(('127.0.0.1', port), timeout=30)
        self.file = self.sock.makefile('rwb'); self.counter = 0
        self.call('auth', {'token': Path(token).read_text().strip()})
    def call(self, method, params=None):
        self.counter += 1
        self.file.write((json.dumps({'id':self.counter,'method':method,'params':params or {}})+'\n').encode()); self.file.flush()
        line = self.file.readline()
        if not line: raise RuntimeError('control disconnected')
        value = json.loads(line)
        if not value.get('ok'): raise RuntimeError(f'{method}: {value.get("error")}')
        return value.get('result')
    def command(self, command, params=None): return self.call('engine.execute', {'command':command,'params':params or {}})
    def close(self): self.file.close(); self.sock.close()

def wait_client(port, token):
    deadline=time.monotonic()+60
    while time.monotonic()<deadline:
        try: return Client(port,token)
        except (OSError,FileNotFoundError): time.sleep(.1)
    raise RuntimeError(f'client {port} did not start')

def prepare(args):
    out=Path(args.output).resolve();out.mkdir(parents=True,exist_ok=True)
    ports=json.loads(Path(args.ports).read_text())
    if len(ports)!=4 or any(type(port) is not int or not 1024<=port<=65535 for port in ports) or len(set(ports))!=4: raise ValueError('four distinct registered ports in1024..65535 required')
    names=['Overlap A — coral','Overlap B — blue','Overlap C — violet','Overlap D — green']
    colors=['#eb634c','#2689d2','#a457dd','#32a972']; entries=[]; clients=[]
    try:
        for i,port in enumerate(ports):
            config=out/f'config-{i}';config.mkdir(exist_ok=True)
            token=out/f'control-{i}.token';log=(out/f'client-{i}.log').open('ab')
            env=dict(os.environ,PHOTOCRAFT_CONFIG_DIR=str(config))
            proc=subprocess.Popen([args.app,'--control',str(port),'--control-token-file',str(token),'--automation-read-root',str(out),'--automation-write-root',str(out)],env=env,stdout=log,stderr=log,start_new_session=True);log.close()
            entry={'port':port,'token':str(token),'pid':proc.pid,'name':names[i]};entries.append(entry)
            c=wait_client(port,token);clients.append(c)
            c.call('ui.resize',{'width':720,'height':440,'x':(i%2)*720,'y':32+(i//2)*440})
            c.call('ui.set',{'panels':{'layers':False,'color':False,'properties':False,'history':False,'options_bar':False},'collaboration':{'name':names[i],'color':[[235,99,76],[38,137,210],[164,87,221],[50,169,114]][i]}})
            c.command('tools.setColors',{'foreground':colors[i]})
        code=args.join_code
        if code:
            for i,c in enumerate(clients):c.command('collab.room.join',{'code':code,'name':names[i],'signalingUrl':args.signaling})
        else:
            if args.document:
                clients[0].call('app.open',{'path':args.document})
            else:
                clients[0].command('file.new',{'name':'Four-artist overlap stress','width':args.width,'height':args.height,'background':'white'})
            clients[0].command('layer.new.layer',{'name':'Shared paint and erase'})
            clients[0].command('collab.room.create',{'name':names[0],'signalingUrl':args.signaling})
            deadline=time.monotonic()+30
            while time.monotonic()<deadline:
                state=clients[0].call('ui.inspect')['collaboration']
                if state['ui']['transport_status'].startswith('Hosting') and state.get('room'):
                    code=state['ui']['code'];break
                time.sleep(.1)
            if not code:raise RuntimeError('host admission timeout')
            for i in range(1,4):clients[i].command('collab.room.join',{'code':code,'name':names[i],'signalingUrl':args.signaling})
        deadline=time.monotonic()+60
        while time.monotonic()<deadline:
            if all(c.call('ui.inspect')['collaboration']['ui']['transport_status'].startswith('Connected') for c in (clients if args.join_code else clients[1:])):break
            time.sleep(.1)
        else:raise RuntimeError('guest bootstrap timeout')
        # Admission can precede receipt/application of a large host checkpoint.
        # Do not start generators while bootstrap/canonical work is still queued.
        deadline=time.monotonic()+120;stable=0;last=None
        while time.monotonic()<deadline:
            states=[c.call('ui.inspect') for c in clients]
            revisions=[s['collaboration']['revision'] for s in states]
            ready=all(s['collaboration'].get('room') and s.get('document') for s in states)
            metrics=[s['collaboration']['ui'].get('transport_metrics') or {} for s in states]
            queued=any(any(m.get(k,0) for k in ['incomingEventsQueued','canonicalOpsQueued','clientOpsQueued','outgoingJobs','bufferedRequests']) for m in metrics)
            stable=stable+1 if ready and not queued and len(set(revisions))==1 and revisions==last else 0;last=revisions
            if stable>=3:break
            errors=[s['collaboration']['ui'].get('error') for s in states]
            if any(errors):raise RuntimeError('bootstrap failed: '+str(errors))
            time.sleep(.15)
        else:raise RuntimeError('guest canonical bootstrap did not settle')
        for c in clients:c.call('ui.set',{'fit':True})
        sizes={(s['document']['width'],s['document']['height']) for s in states}
        if len(sizes)!=1:raise RuntimeError('clients disagree on imported canvas dimensions')
        width,height=next(iter(sizes))
        data={'clients':entries,'code':code,'width':width,'height':height,'app':args.app,'preparedUTC':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime())}
        (out/'native-session.json').write_text(json.dumps(data,indent=2));print(json.dumps({'room':code,'pids':[e['pid'] for e in entries],'session':str(out/'native-session.json')}))
    except Exception:
        for e in entries:
            try:os.kill(e['pid'],15)
            except ProcessLookupError:pass
        raise
    finally:
        for c in clients:c.close()

def percentiles(values):
    values=sorted(values)
    return {k:values[min(len(values)-1,math.ceil(p*len(values))-1)] if values else None for k,p in [('p50',.5),('p95',.95),('p99',.99),('max',1)]}

def decoded_png(path):
    data=Path(path).read_bytes()
    if data[:8]!=b'\x89PNG\r\n\x1a\n':raise ValueError('not PNG')
    at=8;packed=[];header=None
    while at+12<=len(data):
        count=struct.unpack('>I',data[at:at+4])[0];kind=data[at+4:at+8];payload=data[at+8:at+8+count];at+=count+12
        if kind==b'IHDR':header=struct.unpack('>IIBBBBB',payload)
        if kind==b'IDAT':packed.append(payload)
        if kind==b'IEND':break
    if not header:raise ValueError('missing PNG header')
    w,h,depth,color,compression,filtering,interlace=header
    if depth!=8 or color not in (2,6) or interlace!=0 or w*h>24_000_000:raise ValueError('test oracle requires bounded noninterlaced RGB/RGBA8')
    # Prefer the native PNG decoder when installed: hashing four large exports
    # should not keep controls idle for minutes. The standard-library oracle
    # remains available for a dependency-free developer environment.
    try:
        from PIL import Image
    except ImportError:
        Image=None
    if Image is not None:
        with Image.open(path) as source:
            if source.size!=(w,h) or source.mode not in ('RGB','RGBA'):
                raise ValueError('PNG decoder does not match bounded RGB/RGBA8 header')
            decoded=source.convert('RGBA')
            ink=any(low!=255 for low,high in source.convert('RGB').getextrema())
            return hashlib.sha256(decoded.tobytes()).hexdigest(),ink
    channels=3 if color==2 else 4;stride=w*channels;raw=zlib.decompress(b''.join(packed));previous=bytearray(stride);rgba=bytearray()
    if len(raw)!=h*(stride+1):raise ValueError('invalid PNG row length')
    def paeth(a,b,c):
        p=a+b-c;pa,pb,pc=abs(p-a),abs(p-b),abs(p-c)
        return a if pa<=pb and pa<=pc else b if pb<=pc else c
    for y in range(h):
        pos=y*(stride+1);mode=raw[pos];row=bytearray(raw[pos+1:pos+stride+1])
        if mode>4:raise ValueError('invalid PNG filter')
        for x in range(stride):
            a=row[x-channels] if x>=channels else 0;b=previous[x];c=previous[x-channels] if x>=channels else 0
            delta=0 if mode==0 else a if mode==1 else b if mode==2 else (a+b)//2 if mode==3 else paeth(a,b,c)
            row[x]=(row[x]+delta)&255
        for x in range(0,stride,channels):rgba.extend(row[x:x+3]);rgba.append(row[x+3] if channels==4 else 255)
        previous=row
    ink=any(rgba[x:x+3]!=b'\xff\xff\xff' for x in range(0,len(rgba),4))
    return hashlib.sha256(rgba).hexdigest(),ink

def run(args):
    out=Path(args.output).resolve();session=json.loads((out/'native-session.json').read_text());width,height=session['width'],session['height'];start=time.monotonic();barrier=__import__('threading').Barrier(4)
    def artist(i):
        c=None;latencies=[];end_latencies=[];history_latencies=[];frames=[];errors=[];completed=0;baseline_history=0
        try:
            c=Client(session['clients'][i]['port'],session['clients'][i]['token']);initial=c.call('ui.inspect');paint_layer=initial['document']['activeLayer'];baseline_history=len(initial['collaboration']['history']);barrier.wait(timeout=35)
            for stroke in range(args.strokes):
                c.command('layer.select',{'layer':paint_layer})
                erase=(stroke+i)%3==0
                c.call('ui.set',{'tool':'Eraser' if erase else 'Brush'})
                c.command('tools.setBrush',{'brush':{'size':16+(stroke%4)*8,'hardness':.45 if stroke%2 else .9,'spacing':.12,'opacity':.6+(i%2)*.2,'smoothing':{'amount':.05},'shapeDynamics':{'enabled':True,'size':{'control':'penPressure'}}}})
                phase=(stroke*1.31+i*.77);points=[]
                for n in range(args.points):
                    t=n/max(1,args.points-1)
                    x=width*(.08+.84*(.5+.5*math.sin(t*math.tau*2.1+phase)))
                    y=height*(.08+.84*(.5+.5*math.sin(t*math.tau*3.2+phase*.7)))
                    points.append({'kind':'down' if n==0 else 'move','x':x,'y':y,'pressure':.2+.75*(.5+.5*math.sin(t*math.tau*2+i)),'tiltX':25*math.sin(t*math.tau),'tiltY':15,'rotation':t*180})
                for offset in range(0,len(points),args.chunk):
                    t0=time.monotonic();c.call('ui.pointer',{'events':points[offset:offset+args.chunk]});latencies.append((time.monotonic()-t0)*1000)
                    at=points[min(offset+args.chunk-1,len(points)-1)];c.command('collab.cursor',{'position':[at['x'],at['y']],'tool':'eraser' if erase else 'brush'})
                    if args.layer_churn and stroke==0 and offset < len(points)//2 <= offset+args.chunk:
                        c.command('layer.new.layer',{'name':f'Independent layer from artist {i}'})
                        c.command('layer.select',{'layer':paint_layer})
                    if args.step_ms:time.sleep(args.step_ms/1000)
                last=points[-1];t0=time.monotonic();c.call('ui.pointer',{'events':[{'kind':'up','x':last['x'],'y':last['y']}]});end_latencies.append((time.monotonic()-t0)*1000);completed+=1
                if args.layer_churn and stroke==0:
                    # Wait for our creation ACK before reselecting: old clients may
                    # auto-select its layer late. All following gestures must overlap.
                    deadline=time.monotonic()+30
                    while time.monotonic()<deadline:
                        state=c.call('ui.inspect')['collaboration']
                        if len(state['history'])>=baseline_history+2:break
                        if state['ui'].get('error'):raise RuntimeError(state['ui']['error'])
                        time.sleep(.02)
                    else:raise RuntimeError('first stroke and independent layer did not acknowledge')
                    c.command('layer.select',{'layer':paint_layer})
                if stroke%9==8:
                    for command in ['edit.undo','edit.redo']:
                        t0=time.monotonic();c.command(command);history_latencies.append({'command':command,'stroke':stroke+1,'ms':(time.monotonic()-t0)*1000})
                print(json.dumps({'artist':i,'completedStroke':completed,'elapsed':time.monotonic()-start}),flush=True)
                if stroke%4==0:
                    state=c.call('ui.inspect');frames.append({'elapsed':time.monotonic()-start,'fps':state['perf']['fps'],'revision':state['collaboration']['revision'],'timings':state['perf']['timings'],'transport':state['collaboration']['ui'].get('transport_metrics'),'error':state['collaboration']['ui'].get('error')})
                    if state['collaboration']['ui'].get('error'):raise RuntimeError(state['collaboration']['ui']['error'])
        except Exception as error:
            errors.append(str(error));barrier.abort()
        finally:
            if c:c.close()
        return {'artist':i,'baselineHistoryCount':baseline_history,'completedStrokes':completed,'pointerRoundtripMs':percentiles(latencies),'strokeEndRoundtripMs':percentiles(end_latencies),'historyRoundtripMs':percentiles([h['ms'] for h in history_latencies]),'historyOperations':history_latencies,'requests':len(latencies),'frames':frames,'errors':errors}
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:results=list(pool.map(artist,range(4)))
    errors=sum((r['errors'] for r in results),[]);drawing_wall=time.monotonic()-start;clients=[];revisions=[];hashes=[];ink=[];states=[];quiescent=False
    try:
        clients=[Client(e['port'],e['token']) for e in session['clients']]
        deadline=time.monotonic()+120;stable=0;last=None
        while time.monotonic()<deadline:
            states=[]
            for c in clients:
                remaining=deadline-time.monotonic()
                if remaining<=0:break
                c.sock.settimeout(min(3,remaining));states.append(c.call('ui.inspect'))
            if len(states)!=4:break
            revisions=[s['collaboration']['revision'] for s in states]
            rooms=[s['collaboration'].get('room') for s in states]
            if not all(rooms):errors.append('a client lost its room');break
            room_errors=[s['collaboration']['ui'].get('error') for s in states]
            if any(room_errors):errors.extend(e for e in room_errors if e);break
            metrics=[s['collaboration']['ui'].get('transport_metrics') or {} for s in states]
            queued=any(any(m.get(k,0) for k in ['incomingEventsQueued','canonicalOpsQueued','clientOpsQueued','outgoingJobs','bufferedRequests']) for m in metrics)
            equal=len(set(revisions))==1 and revisions[0]>0 and not queued
            stable=stable+1 if equal and revisions==last else 0;last=revisions
            if stable>=3:quiescent=True;break
            time.sleep(.15)
        if not quiescent:errors.append('canonical queues did not converge before deadline')
        # Let a concurrently started native recording finish before export warnings appear.
        remaining=args.export_after_seconds-(time.monotonic()-start)
        if remaining>0 and quiescent and not errors:time.sleep(remaining)
        for i,c in enumerate(clients):
            # Pixel hashing can outlast the control server's idle timeout.
            # Give each export its own freshly authenticated connection.
            c.close()
            c=Client(session['clients'][i]['port'],session['clients'][i]['token'])
            clients[i]=c
            c.sock.settimeout(30)
            if i<len(states):
                count=len(states[i]['collaboration']['history'])
                expected=results[i]['baselineHistoryCount']+args.strokes+(1 if args.layer_churn else 0)
                if count!=expected:errors.append(f'artist{i} history count{count}, expected{expected}')
            try:
                c.call('app.save',{'path':f'final-{i}.png'})
                if not args.no_window_screenshots:c.call('ui.screenshot',{'path':f'final-window-{i}.png','focus':False})
                c.close()
                h,has_ink=decoded_png(out/f'final-{i}.png');hashes.append(h);ink.append(has_ink)
            except Exception as error:errors.append(f'export artist{i}: {error}')
    except Exception as error:errors.append(str(error))
    finally:
        for c in clients:c.close()
    result={'wallSeconds':time.monotonic()-start,'drawingWallSeconds':drawing_wall,'strokesPerArtist':args.strokes,'pointsPerStroke':args.points,'chunkPoints':args.chunk,'layerChurn':args.layer_churn,'artists':results,'revisions':revisions,'rgbaHashes':hashes,'hasInk':ink,'quiescent':quiescent,'converged':len(hashes)==4 and len(set(hashes))==1 and all(ink) and quiescent,'errors':errors,'latencyMeaning':'ui.pointer control roundtrip, not pointer-to-photon latency'}
    (out/'native-stress-results.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
    if errors or not result['converged'] or any(r['completedStrokes']!=args.strokes for r in results):raise RuntimeError('native stress failed; see results')

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='mode',required=True)
    q=sub.add_parser('prepare');q.add_argument('--app',required=True);q.add_argument('--output',required=True);q.add_argument('--ports',required=True);q.add_argument('--signaling',default='http://127.0.0.1:5548');q.add_argument('--width',type=int,default=1024);q.add_argument('--height',type=int,default=640);q.add_argument('--document',help='Test document relative to the prepared automation read root');q.add_argument('--join-code',help='Join an existing browser-host room with all four artists instead of creating a native host')
    q=sub.add_parser('run');q.add_argument('--output',required=True);q.add_argument('--strokes',type=int,default=64);q.add_argument('--points',type=int,default=96);q.add_argument('--chunk',type=int,default=8);q.add_argument('--step-ms',type=float,default=15);q.add_argument('--export-after-seconds',type=float,default=0);q.add_argument('--no-window-screenshots',action='store_true');q.add_argument('--layer-churn',action='store_true',help='Create an independent layer during each artist first overlapping stroke, then reselect the shared paint layer')
    args=p.parse_args()
    if args.mode=='run' and not (1<=args.strokes<=256 and 2<=args.points<=4096 and 1<=args.chunk<=256 and 0<=args.step_ms<=1000 and 0<=args.export_after_seconds<=600):p.error('bounded positive stroke/point/chunk counts required')
    if args.mode=='prepare' and not (64<=args.width<=8192 and 64<=args.height<=8192 and args.width*args.height<=24_000_000):p.error('bounded canvas64..8192 required')
    (prepare if args.mode=='prepare' else run)(args)
