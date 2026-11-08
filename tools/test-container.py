#!/usr/bin/env python3
"""Prove the image's default report against a local, read-only fake forge."""
import argparse
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--engine", default="podman", choices=["podman", "docker"])
parser.add_argument("image")
args = parser.parse_args()
seen = []


class Forge(BaseHTTPRequestHandler):
    def do_GET(self):
        seen.append(self.path)
        if self.path.split("?")[0] != "/user/repos":
            self.send_error(404)
            return
        body = b"[]"
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Forge)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    result = subprocess.run(
        [args.engine, "run", "--rm", "--network=host", "--read-only",
         "-e", "ALMANAC_OWNER=example", "-e", "GITHUB_TOKEN=fixture",
         "-e", f"GITHUB_API_URL=http://127.0.0.1:{server.server_port}", args.image],
        check=True, capture_output=True, text=True,
    )
    assert result.stdout.strip() == "0 repositories scanned:", result.stdout
    assert len(seen) == 1 and seen[0].startswith("/user/repos?"), seen
    subprocess.run([args.engine, "run", "--rm", "--network=none", "--read-only",
                    args.image, "synth"], check=True)
    print("image default report and offline synth passed")
finally:
    server.shutdown()
    server.server_close()
