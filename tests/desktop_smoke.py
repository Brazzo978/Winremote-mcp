"""Real local input test against an owned temporary Tk fixture. Never touches user documents."""
import argparse, base64, ctypes, hashlib, json, os, queue, struct, subprocess, sys, tempfile, time
from pathlib import Path
from smoke import reader, next_line, start_host, stop

def fixture(state_path):
    import tkinter as tk
    from ctypes import wintypes
    ctypes.WinDLL('user32').SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
    root=tk.Tk()
    root.title('Winremote input test - temporary')
    root.geometry('620x440+100+100')
    root.attributes('-topmost',True)
    tk.Label(root,text='Temporary Winremote test: no user files',font=('Segoe UI',12)).pack()
    text=tk.Text(root,height=9,width=70)
    text.pack(padx=15,pady=10)
    canvas=tk.Canvas(root,height=170,bg='#dceaff')
    canvas.pack(fill='both',expand=True,padx=15,pady=10)
    canvas.create_text(280,80,text='Mouse / wheel test area',font=('Segoe UI',14))
    events={'left':0,'right':0,'middle':0,'double':0,'vertical':0,'horizontal':0,'keys':[]}
    for sequence,key in [('<Button-1>','left'),('<Button-3>','right'),('<Button-2>','middle'),('<Double-Button-1>','double')]:
        canvas.bind(sequence,lambda event,k=key:events.__setitem__(k,events[k]+1))
    def wheel(event):
        events['vertical']+=event.delta
        return 'break'
    canvas.bind('<MouseWheel>',wheel)
    def select_all(event):
        text.tag_add('sel','1.0','end-1c')
        return 'break'
    text.bind('<Control-a>',select_all)
    text.bind('<KeyPress>',lambda e:events['keys'].append(e.keysym))
    # Tk has no portable binding for WM_MOUSEHWHEEL. Observe the native message
    # on the fixture canvas; retain the original window procedure for Tk.
    user=ctypes.WinDLL('user32',use_last_error=True)
    callback_type=ctypes.WINFUNCTYPE(ctypes.c_ssize_t,wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM)
    user.SetWindowLongPtrW.argtypes=[wintypes.HWND,ctypes.c_int,ctypes.c_void_p]
    user.SetWindowLongPtrW.restype=ctypes.c_void_p
    user.CallWindowProcW.argtypes=[ctypes.c_void_p,wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM]
    user.CallWindowProcW.restype=ctypes.c_ssize_t
    root.update()
    user.GetParent.argtypes=[wintypes.HWND]
    user.GetParent.restype=wintypes.HWND
    originals={}
    callbacks=[]
    for target in {canvas.winfo_id(),text.winfo_id(),root.winfo_id(),user.GetParent(root.winfo_id())}:
        if not target:continue
        def make_callback(target):
            @callback_type
            def window_proc(hwnd,msg,wparam,lparam):
                if msg==0x020E:
                    events['horizontal']+=ctypes.c_short((wparam>>16)&0xffff).value
                    return 0
                return user.CallWindowProcW(originals[target],hwnd,msg,wparam,lparam)
            return window_proc
        callback=make_callback(target)
        callbacks.append(callback)
        originals[target]=user.SetWindowLongPtrW(target,-4,ctypes.cast(callback,ctypes.c_void_p))
        assert originals[target]
    root.lift()
    root.focus_force()
    text.focus_force()
    def snapshot():
        data={'events':events,'text':text.get('1.0','end-1c'),'selection':len(text.tag_ranges('sel'))>0,
              'canvas_x':canvas.winfo_rootx()+canvas.winfo_width()//2,'canvas_y':canvas.winfo_rooty()+canvas.winfo_height()//2,
              'text_x':text.winfo_rootx()+50,'text_y':text.winfo_rooty()+30}
        temp=state_path.with_suffix('.tmp')
        temp.write_text(json.dumps(data,ensure_ascii=True),encoding='utf-8')
        os.replace(temp,state_path)
        root.after(40,snapshot)
    snapshot()
    print('READY',flush=True)
    try:root.mainloop()
    finally:
        for target,original in originals.items():user.SetWindowLongPtrW(target,-4,original)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--exe',type=Path)
    parser.add_argument('--allow-headless',action='store_true')
    parser.add_argument('--fixture',type=Path)
    args=parser.parse_args()
    if args.fixture:return fixture(args.fixture)
    assert args.exe
    exe=args.exe.resolve()
    user=ctypes.WinDLL('user32')
    user.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
    from ctypes import wintypes
    user.GetForegroundWindow.restype=wintypes.HWND
    user.SetForegroundWindow.argtypes=[wintypes.HWND]
    user.GetCursorPos.argtypes=[ctypes.POINTER(wintypes.POINT)]
    user.SetCursorPos.argtypes=[ctypes.c_int,ctypes.c_int]
    user.GetAsyncKeyState.argtypes=[ctypes.c_int]
    user.GetAsyncKeyState.restype=ctypes.c_short
    user.keybd_event.argtypes=[ctypes.c_ubyte,ctypes.c_ubyte,wintypes.DWORD,ctypes.c_size_t]
    original_focus=user.GetForegroundWindow()
    original_cursor=wintypes.POINT()
    user.GetCursorPos(ctypes.byref(original_cursor))
    processes=[]
    with tempfile.TemporaryDirectory(prefix='winremote-desktop-test-') as work:
        work=Path(work)
        with (work/'stderr.txt').open('w',encoding='utf-8') as diagnostic:
            try:
                host,invite,connection=start_host(exe,180,work,diagnostic)
                processes.append(host)
                credential=work/'connection.json'
                pair=subprocess.run([str(exe),'pair','--connection',str(credential)],input=invite+'\n',capture_output=True,encoding='utf-8',timeout=15)
                assert pair.returncode==0,'Pair failed'
                mcp=subprocess.Popen([str(exe),'mcp','--connection',str(credential)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=diagnostic,text=True,encoding='utf-8')
                processes.append(mcp)
                replies=reader(mcp.stdout);seq=0
                def rpc(method,params=None,notify=False):
                    nonlocal seq
                    seq+=1;msg={'jsonrpc':'2.0','method':method}
                    if not notify:msg['id']=seq
                    if params is not None:msg['params']=params
                    mcp.stdin.write(json.dumps(msg)+'\n');mcp.stdin.flush()
                    if notify:return
                    result=json.loads(next_line(replies,30))
                    assert result['id']==seq,'RPC id mismatch'
                    if 'error' in result:return {'isError':True,'rpcError':result['error']}
                    return result['result']
                rpc('initialize',{'protocolVersion':'2025-11-25','capabilities':{},'clientInfo':{'name':'desktop-test','version':'1'}})
                rpc('notifications/initialized',notify=True)
                assert len(rpc('tools/list')['tools'])==18
                def raw(name,arguments):return rpc('tools/call',{'name':name,'arguments':arguments})
                def call(name,arguments):
                    r=raw(name,arguments)
                    assert not r.get('isError'),'Failed '+name
                    return r['structuredContent']
                screenshot=raw('desktop_screenshot',{'max_width':1280})
                if screenshot.get('isError'):
                    assert args.allow_headless,'Desktop unavailable'
                    print('SKIP desktop input: no unlocked interactive desktop',flush=True)
                    return
                fixture_proc=subprocess.Popen([sys.executable,'-u',str(Path(__file__).resolve()),'--fixture',str(work/'state.json')],stdout=subprocess.PIPE,stderr=diagnostic,text=True,encoding='utf-8')
                processes.append(fixture_proc)
                assert next_line(reader(fixture_proc.stdout),15).strip()=='READY'
                def state():
                    for _ in range(20):
                        try:return json.loads((work/'state.json').read_text(encoding='utf-8'))
                        except (OSError,json.JSONDecodeError):time.sleep(.02)
                    raise AssertionError('Fixture state unavailable')
                def wait_for(predicate,label):
                    deadline=time.monotonic()+3
                    while time.monotonic()<deadline:
                        value=state()
                        if predicate(value):return value
                        time.sleep(.04)
                    raise AssertionError(label)
                current=state()
                pos={'x':current['canvas_x'],'y':current['canvas_y']}
                call('desktop_move',pos)
                point=wintypes.POINT()
                user.GetCursorPos(ctypes.byref(point))
                assert abs(point.x-pos['x'])<=1 and abs(point.y-pos['y'])<=1,'Physical cursor mapping mismatch'
                print('PASS physical cursor coordinates',flush=True)
                for button,kind in [('left','left'),('right','right'),('middle','middle')]:
                    previous=state()['events'][kind]
                    call('desktop_click',{**pos,'button':button})
                    wait_for(lambda s:s['events'][kind]>previous,'Click not received')
                previous=state()['events']['double']
                call('desktop_click',{**pos,'clicks':2})
                wait_for(lambda s:s['events']['double']>previous,'Double click not received')
                print('PASS left/right/middle and double click delivery',flush=True)
                vertical=state()['events']['vertical']
                call('desktop_scroll',{'vertical':-2})
                wait_for(lambda s:s['events']['vertical']==vertical-240,'Vertical wheel not received')
                horizontal=state()['events']['horizontal']
                call('desktop_scroll',{'horizontal':3})
                wait_for(lambda s:s['events']['horizontal']==horizontal+360,'Horizontal wheel not received')
                print('PASS vertical/horizontal wheel direction and notches',flush=True)
                current=state()
                call('desktop_click',{'x':current['text_x'],'y':current['text_y']})
                content='Ciao \u00e8 \u00f1 \u6f22\u5b57 \U0001f600\nseconda riga\tfine'
                call('desktop_type',{'text':content})
                wait_for(lambda s:s['text']==content,'Unicode text mismatch')
                print('PASS Unicode, supplementary character, newline and tab delivery',flush=True)
                call('desktop_key',{'keys':['CTRL','A']})
                wait_for(lambda s:s['selection'],'Chord did not select fixture text')
                call('desktop_key',{'keys':['BACKSPACE']})
                wait_for(lambda s:s['text']=='','Backspace not received')
                assert not any(user.GetAsyncKeyState(vk)&0x8000 for vk in (0x10,0x11,0x12,0x5B,0x5C)), 'Modifier left down'
                print('PASS key chord, single key and released modifiers',flush=True)
                for vk,tool,arguments in [(0xA2,'desktop_type',{'text':'should not appear'}),(0x87,'desktop_key',{'keys':['F24']})]:
                    assert not user.GetAsyncKeyState(vk)&0x8000,'Fixture test key is already held'
                    user.keybd_event(vk,0,0,0)
                    try:
                        deadline=time.monotonic()+1
                        while not user.GetAsyncKeyState(vk)&0x8000 and time.monotonic()<deadline:time.sleep(.01)
                        assert user.GetAsyncKeyState(vk)&0x8000,'Fixture key down failed'
                        assert raw(tool,arguments).get('isError'),'Held key accepted'
                    finally:user.keybd_event(vk,0,2,0)
                wait_for(lambda s:s['text']=='','Rejected held-key input changed text')
                print('PASS held modifier and target key rejection',flush=True)
                for tool,arguments in [('desktop_click',{**pos,'clicks':3}),('desktop_scroll',{}),('desktop_type',{'text':''}),('desktop_key',{'keys':['CTRL']}),('desktop_move',{'x':-2147483648,'y':-2147483648})]:
                    result=raw(tool,arguments)
                    assert result.get('isError'),'Invalid input accepted'
                assert state()['text']=='','Rejected inputs changed fixture'
                print('PASS invalid input rejection',flush=True)
                result=raw('desktop_screenshot',{'max_width':1280})
                assert not result.get('isError')
                image=next(c for c in result['content'] if c['type']=='image')
                assert base64.b64decode(image['data']).startswith(b'\x89PNG\r\n\x1a\n')
                print('PASS screenshot after real input',flush=True)
            finally:
                for process in reversed(processes):stop(process)
                user.SetCursorPos(original_cursor.x,original_cursor.y)
                if original_focus:user.SetForegroundWindow(original_focus)
    print('All real desktop input checks passed; fixture and temporary credentials removed.',flush=True)
if __name__=='__main__':main()
