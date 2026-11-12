#!/usr/bin/env python3
"""Start a packaged search daemon and verify its real local IPC protocol."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time


def verify(binary):
    with tempfile.TemporaryDirectory(prefix="atlas-search-test-") as temporary:
        runtime = Path(temporary)
        endpoint = runtime / "atlas-search.sock"
        with (runtime / "daemon.log").open("w+") as log:
            process = subprocess.Popen([str(binary), "--console"], stdout=log,
                                       stderr=subprocess.STDOUT,
                                       env=dict(os.environ, XDG_RUNTIME_DIR=temporary))
            try:
                deadline = time.monotonic() + 20
                while not endpoint.exists():
                    if process.poll() is not None or time.monotonic() >= deadline:
                        log.seek(0)
                        raise RuntimeError("Search daemon did not start: " + log.read())
                    time.sleep(0.1)
                with socket.socket(socket.AF_UNIX) as client:
                    client.settimeout(10)
                    client.connect(str(endpoint))
                    with client.makefile("rwb") as stream:
                        stream.write(b'{"Hello":{"protocol":1}}\n')
                        stream.flush()
                        hello = json.loads(stream.readline())
                        if hello.get("Hello", {}).get("protocol") != 1:
                            raise RuntimeError(f"Wrong search protocol: {hello}")
                        stream.write(b'"Status"\n')
                        stream.flush()
                        response = json.loads(stream.readline())
                        if not response.get("Status"):
                            raise RuntimeError(f"Search daemon did not report its index: {response}")
                        print(json.dumps(dict(hello=hello, status=response)))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    verify(parser.parse_args().binary.resolve())
