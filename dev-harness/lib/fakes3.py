#!/usr/bin/env python3
"""
fakes3.py — a ~60-line fake S3 endpoint for ptm-sync, supporting the three
operations it uses: HEAD, GET and PUT on a single object.

Why this is needed: rust-s3 builds the request host as `<bucket>.<region-host>`
and `S3Client` never calls `.with_path_style()`, so `ptm-sync` can only reach a
local endpoint if `<bucket>.<host>` resolves. That requires an /etc/hosts entry
(see dev-harness/README.md).

    from fakes3 import FakeS3, hosts_ready
    if hosts_ready("ptm-harness.localhost"):
        with FakeS3() as s3:
            ... env={"AWS_REGION": s3.region, ...}
"""

import hashlib
import os
import socket
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

# The name that must resolve to 127.0.0.1 for a round trip to work.
HARNESS_HOST = "ptm-harness.localhost"
HARNESS_BUCKET = "b"

# Bucket names to try, so a hosts entry for any of them is enough.
CANDIDATE_BUCKETS = ("b", "ptm", "harness", "ptm-sync", "test")


def hosts_ready(bucket=HARNESS_BUCKET, host=HARNESS_HOST):
    """True if `<bucket>.<host>` resolves — i.e. the /etc/hosts entry is in place."""
    try:
        socket.getaddrinfo(f"{bucket}.{host}", None)
        return True
    except socket.gaierror:
        return False


def pick_bucket(host=HARNESS_HOST, candidates=CANDIDATE_BUCKETS):
    """Return the first bucket name whose request host resolves, else None.

    /etc/hosts is exact-match, so the useful entry is `<bucket>.<host>`; which
    bucket you picked is irrelevant to the harness, so accept any of them.
    """
    for bucket in candidates:
        if hosts_ready(bucket, host):
            return bucket
    return None


def hosts_hint(bucket=HARNESS_BUCKET, host=HARNESS_HOST):
    """The exact line(s) needed.

    /etc/hosts does exact matching only — no wildcards — so the *subdomain*
    that rust-s3 actually requests needs its own line. A single entry for
    `host` is not enough.
    """
    return (
        f"needs this /etc/hosts entry (exact match; subdomains are not implied):\n"
        f"      127.0.0.1  {bucket}.{host}\n"
        f"  (rust-s3 requests '{bucket}.{host}' because it uses subdomain-style\n"
        f"   addressing and S3Client never sets path-style)"
    )


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def _etag(self, data):
        return '"%s"' % hashlib.md5(data).hexdigest()

    def _read_store(self):
        p = self.server.store_path
        return open(p, "rb").read() if os.path.exists(p) else None

    def do_HEAD(self):
        data = self._read_store()
        if data is None:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("ETag", self._etag(data))
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()

    def do_GET(self):
        data = self._read_store()
        hook = getattr(self.server, "get_hook", None)
        if hook:
            # Let a harness act while the pull is in flight (e.g. start a
            # concurrent writer) — this is what reproduces the install race.
            # A raising hook must not truncate the response body, or the client
            # sees "unexpected end of file" instead of the behaviour under test.
            try:
                hook()
            except Exception as exc:  # noqa: BLE001
                self.server.hook_errors.append(f"{type(exc).__name__}: {exc}")
        if data is None:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("ETag", self._etag(data))
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_PUT(self):
        n = int(self.headers.get("Content-Length", 0))
        data = self.rfile.read(n)
        with open(self.server.store_path, "wb") as f:
            f.write(data)
        self.server.puts += 1
        self.send_response(200)
        self.send_header("ETag", self._etag(data))
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *args):
        pass  # keep harness output readable


class FakeS3:
    """Context manager running the fake endpoint on a free port."""

    def __init__(self, store_path, port=0):
        self.store_path = str(store_path)
        self.port = port
        self.httpd = None
        self.thread = None
        self._get_hook = None  # callable run at the start of each GET
        self.hook_errors = []  # exceptions raised by the hook, for reporting

    def set_get_hook(self, fn):
        """Run `fn` at the start of every GET.

        Provided for racing the pull (acting while the download is in flight).
        No harness uses it at present: the one race worth testing is undefined
        behaviour and gave different results on different runs, so it is
        reproduced deterministically in wal-safety.py instead. Kept because it
        is the right tool if a deterministic race is ever found.
        """
        self._get_hook = fn
        if self.httpd:
            self.httpd.get_hook = fn

    @property
    def region(self):
        """Value to pass as AWS_REGION so rust-s3 builds Region::Custom."""
        return f"http://{HARNESS_HOST}:{self.port}"

    @property
    def puts(self):
        return self.httpd.puts if self.httpd else 0

    def __enter__(self):
        self.httpd = HTTPServer(("127.0.0.1", self.port), _Handler)
        self.httpd.store_path = self.store_path
        self.httpd.puts = 0
        self.httpd.get_hook = self._get_hook
        self.httpd.hook_errors = self.hook_errors
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *exc):
        if self.httpd:
            self.httpd.shutdown()
            self.httpd.server_close()
        return False


def main():
    """Run standalone for manual poking:  python3 dev-harness/lib/fakes3.py [port]"""
    import tempfile
    store = os.path.join(tempfile.mkdtemp(prefix="fakes3-"), "object.bin")
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9001
    with FakeS3(store, port) as s3:
        print(f"fake S3 on {s3.region}  (store: {store})")
        print(f"AWS_REGION={s3.region} AWS_ACCESS_KEY_ID=x AWS_SECRET_ACCESS_KEY=y")
        if not hosts_ready():
            print(f"\nWARNING: {hosts_hint()}")
        try:
            threading.Event().wait()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
