#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""存档管理面板端到端测试:进村→展开面板→浏览→立即保存(持久化)→导出(下载校验)
→刷新页面→验证从 IndexedDB 恢复(__saveSource 变'玩家进度')。截图面板。
用法:python3.11 test-savepanel.py --dir <release目录> [--port 8836]
"""
import argparse, http.server, json, os, socketserver, threading, urllib.parse

CT = {".js":"text/javascript",".wasm":"application/wasm",".data":"application/octet-stream",
      ".ipa":"application/octet-stream",".html":"text/html; charset=utf-8",
      ".dat":"application/octet-stream",".plist":"application/octet-stream"}

def make_handler(root):
    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self,*a): pass
        def do_GET(self):
            path=urllib.parse.urlparse(self.path).path
            if path in ("/",""): path="/index.html"
            fp=os.path.normpath(os.path.join(root,path.lstrip("/")))
            if not fp.startswith(os.path.abspath(root)) or not os.path.isfile(fp):
                self.send_response(404); self.end_headers(); self.wfile.write(b"nf"); return
            with open(fp,"rb") as f: body=f.read()
            self.send_response(200)
            self.send_header("Content-Type",CT.get(os.path.splitext(fp)[1],"application/octet-stream"))
            self.send_header("Content-Length",str(len(body)))
            for h in (("Cache-Control","no-store"),("Cross-Origin-Opener-Policy","same-origin"),
                      ("Cross-Origin-Embedder-Policy","require-corp"),("Cross-Origin-Resource-Policy","cross-origin")):
                self.send_header(*h)
            self.end_headers(); self.wfile.write(body)
    return H

class TS(socketserver.ThreadingMixIn,http.server.HTTPServer):
    daemon_threads=True; allow_reuse_address=True

FS_LIST = """() => {
  if (!window.Module || !Module.FS) return null;
  const SBOX='/touchHLE_sandbox/com.taomee.MoleWorld'; const out=[];
  (function walk(d){let es;try{es=Module.FS.readdir(d);}catch(e){return;}
   for(const e of es){if(e==='.'||e==='..')continue;const p=d+'/'+e;let st;
    try{st=Module.FS.stat(p);}catch(_){continue;}
    if(Module.FS.isDir(st.mode))walk(p);else out.push({path:p.slice(SBOX.length+1),len:Module.FS.readFile(p).length});}})(SBOX);
  return out;
}"""

IDB_GET = """() => new Promise((res)=>{
  const rq=indexedDB.open('moleworld-saves',1);
  rq.onsuccess=()=>{const db=rq.result; let st;
    try{st=db.transaction('kv').objectStore('kv');}catch(e){return res({err:'no-store'});}
    const g=st.get('player');
    g.onsuccess=()=>res(g.result?{files:g.result.files.map(f=>({path:f.path,len:f.data.length})),source:g.result.source}:null);
    g.onerror=()=>res({err:'get'});};
  rq.onerror=()=>res({err:'open'});
})"""

def main():
    ap=argparse.ArgumentParser()
    ap.add_argument("--dir",required=True); ap.add_argument("--wait",type=float,default=44)
    ap.add_argument("--port",type=int,default=8836)
    args=ap.parse_args()
    root=os.path.abspath(args.dir)
    httpd=TS(("127.0.0.1",args.port),make_handler(root))
    threading.Thread(target=httpd.serve_forever,daemon=True).start()
    url=f"http://127.0.0.1:{args.port}/index.html"
    from playwright.sync_api import sync_playwright
    P=lambda *a: print(*a, flush=True)
    with sync_playwright() as p:
        browser=p.chromium.launch(headless=False,args=["--ignore-gpu-blocklist","--enable-webgl"])
        ctx=browser.new_context(viewport={"width":1200,"height":860},accept_downloads=True)
        page=ctx.new_page()
        errs=[]
        page.on("console", lambda m: errs.append(m.text) if m.type=="error" else None)
        page.on("pageerror", lambda e: errs.append("PAGEERROR: "+str(e)))
        P(f"[1] 打开 {url},等 {args.wait}s 进村…")
        page.goto(url,wait_until="commit")
        page.wait_for_timeout(int(args.wait*1000))

        ready=page.evaluate("() => !!window.__runtimeReady")
        src=page.evaluate("() => window.__saveSource")
        P(f"[2] runtimeReady={ready}  saveSource={src!r}(首次应为'默认种子')")

        files=page.evaluate(FS_LIST)
        P(f"[3] 沙盒文件({len(files) if files else 0}):")
        for f in (files or []): P(f"      {f['path']}  {f['len']}B")

        # 展开面板(确定性:点一下看 open class 是否切换;若没展开再点一次,确保后续浏览/截图可见)
        opened=page.evaluate("""()=>{const t=document.getElementById('stoggle');const p=document.getElementById('savepane');
          const before=p.classList.contains('open'); t.click(); const after=p.classList.contains('open');
          if(!after) t.click(); return before!==after;}""")
        page.wait_for_timeout(300)
        P(f"[4] 面板折叠切换有效={opened}")

        # 浏览
        page.eval_on_selector("#sb-browse","el=>el.click()"); page.wait_for_timeout(400)
        nrows=page.eval_on_selector_all(".sfile", "els => els.length")
        P(f"[5] 存档浏览渲染 {nrows} 个文件行")
        if nrows: page.eval_on_selector(".sfile .fh","el=>el.click()"); page.wait_for_timeout(200)

        # 立即保存(持久化)
        page.eval_on_selector("#sb-save","el=>el.click()"); page.wait_for_timeout(1200)
        idb=page.evaluate(IDB_GET)
        ok_persist = idb and not idb.get('err') and idb.get('files')
        P(f"[6] 立即保存 → IndexedDB: {('存了 '+str(len(idb['files']))+' 文件, source='+str(idb.get('source'))) if ok_persist else idb}")

        # 导出(捕获下载并校验)
        P("[7] 点导出,捕获下载…")
        try:
            with page.expect_download(timeout=8000) as di:
                page.eval_on_selector("#sb-export","el=>el.click()")
            dl=di.value; path=dl.path()
            with open(path,'rb') as fh: bundle=json.load(fh)
            ok_export = bundle.get('format')=='moleworld-save' and isinstance(bundle.get('files'),list) and bundle['files']
            P(f"      下载名={dl.suggested_filename}  format={bundle.get('format')}  files={len(bundle.get('files',[]))}  有效={bool(ok_export)}")
        except Exception as e:
            ok_export=False; P(f"      导出失败: {e}")

        # 截图面板
        page.screenshot(path="/tmp/savepanel.png")
        P("[8] 截图 -> /tmp/savepanel.png")

        # 刷新 → 应从 IndexedDB 恢复
        P(f"[9] 刷新页面,等 {args.wait}s,验证从持久化恢复…")
        page.reload(wait_until="commit"); page.wait_for_timeout(int(args.wait*1000))
        src2=page.evaluate("() => window.__saveSource")
        ready2=page.evaluate("() => !!window.__runtimeReady")
        P(f"      刷新后 runtimeReady={ready2}  saveSource={src2!r}(应为'玩家进度'=从持久化恢复)")

        ctx.close(); browser.close()
    httpd.shutdown()
    P("\n===== 结论 =====")
    P(f"  运行时就绪      : {'✓' if ready and ready2 else '✗'}")
    P(f"  沙盒可读        : {'✓ '+str(len(files))+'文件' if files else '✗'}")
    P(f"  面板展开        : {'✓' if opened else '✗'}")
    P(f"  存档浏览        : {'✓ '+str(nrows)+'行' if nrows else '✗'}")
    P(f"  立即保存/持久化 : {'✓' if ok_persist else '✗'}")
    P(f"  导出下载校验    : {'✓' if ok_export else '✗'}")
    P(f"  刷新恢复(关键)  : {'✓ '+repr(src2) if src2=='玩家进度' else '✗ '+repr(src2)}")
    P(f"  控制台错误      : {len(errs)} 条" + (("  → "+errs[0][:160]) if errs else ""))

if __name__=="__main__":
    main()
