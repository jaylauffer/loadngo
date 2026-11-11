#!/usr/bin/env python3
"""Serves the motion-matching browser benchmark and collects its result.

  python3 animation/bench/serve.py --db walk.lmm --out result.json [--port 5490] [--timeout 300]

Serves animation/bench/ (index.html, bench.js, pkg/ from animation/pkg) cross-origin
isolated (COOP/COEP, so performance.now() resolves microseconds), and db.lmm from --db.
When the page POSTs /result, writes it to --out and exits 0; exits 1 on --timeout.
Binds 127.0.0.1 only. Point a browser at http://127.0.0.1:<port>/ and wait.
"""
import argparse
import http.server
import json
import os
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
PKG = os.path.join(os.path.dirname(HERE), "pkg")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", required=True, help="the database file anim_build wrote (served as db.lmm)")
    ap.add_argument("--out", required=True, help="where to write the page's result (JSON)")
    ap.add_argument("--port", type=int, default=5490, help="port on 127.0.0.1 (default 5490)")
    ap.add_argument("--timeout", type=float, default=300, help="seconds to wait for a result (default 300)")
    a = ap.parse_args()
    done = threading.Event()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def end_headers(self):
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
            self.send_header("Cache-Control", "no-store")
            super().end_headers()

        def translate_path(self, path):
            p = path.split("?", 1)[0]
            if p == "/db.lmm":
                return os.path.abspath(a.db)
            if p.startswith("/pkg/"):
                return os.path.join(PKG, p[len("/pkg/"):])
            return os.path.join(HERE, p.lstrip("/") or "index.html")

        def guess_type(self, path):
            if path.endswith(".wasm"):
                return "application/wasm"
            if path.endswith(".js"):
                return "text/javascript"
            return super().guess_type(path)

        def do_POST(self):
            if self.path != "/result":
                self.send_error(404)
                return
            body = self.rfile.read(int(self.headers.get("content-length", "0")))
            with open(a.out, "wb") as f:
                f.write(body)
            self.send_response(204)
            self.end_headers()
            try:
                print(json.dumps(json.loads(body), indent=1)[:4000])
            except ValueError:
                print(body[:2000])
            done.set()

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"serving http://127.0.0.1:{a.port}/", flush=True)
    ok = done.wait(a.timeout)
    server.shutdown()
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
