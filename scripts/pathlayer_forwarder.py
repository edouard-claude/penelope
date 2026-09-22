#!/usr/bin/env python3
"""Relais passif Pénélope WS → Pathlayer HTTP ; nécessite websockets>=14."""
import json
import os
import time
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

from websockets.sync.client import connect

def local(url):
    if urlsplit(url).hostname not in {"127.0.0.1", "localhost"}:
        raise SystemExit("WS et ingest doivent rester sur loopback")
    return url

ws_url = local(os.environ.get("PENELOPE_EVENTS_URL", "ws://127.0.0.1:9465/events"))
ingest_url = local(os.environ.get("PATHLAYER_INGEST_URL", "http://127.0.0.1:8787/ingest"))
token = os.environ["PENELOPE_EVENTS_TOKEN"]
cursor = Path(os.environ.get("PENELOPE_EVENTS_CURSOR", ".penelope-pathlayer.cursor"))
last = int(cursor.read_text()) if cursor.exists() else 0
while True:
    try:
        with connect(
            f"{ws_url}?after_id={last}",
            additional_headers={"Authorization": f"Bearer {token}"},
            open_timeout=5,
        ) as stream:
            for raw in stream:
                event = json.loads(raw)
                body = json.dumps({"source": "penelope", "payload": event}).encode()
                request = Request(
                    ingest_url, data=body, headers={"Content-Type": "application/json"}
                )
                with urlopen(request, timeout=5) as response:
                    if response.status != 200:
                        raise RuntimeError(f"ingest HTTP {response.status}")
                last = int(event["event_id"])
                temporary = cursor.with_name(cursor.name + ".tmp")
                fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
                with os.fdopen(fd, "w") as output:
                    output.write(str(last))
                temporary.replace(cursor)
    except KeyboardInterrupt:
        break
    except Exception as error:
        print(f"relais interrompu : {error}", flush=True)
        time.sleep(1)
