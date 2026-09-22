#!/usr/bin/env python3
"""Démo Pathlayer : traducteur Pénélope et détection passive des boucles."""

import os
import signal
import threading

from pathlayer.adapters.http_ingest import HttpIngestAdapter
from pathlayer.detectors.loop import LoopDetector
from pathlayer.events import ToolEvent
from pathlayer.pipeline import Pipeline


def translate(frame):
    if frame.get("kind") != "runtime.tool":
        return []
    payload = frame["payload"]
    return [
        ToolEvent(
            tool=payload["tool"],
            args=payload.get("args") if isinstance(payload.get("args"), dict) else {},
            result=payload.get("result"),
            run_id=frame.get("run_id") or frame.get("session_id"),
            metadata={"event_id": frame["event_id"]},
        )
    ]


pipelines = {}


def observe(event):
    pipeline = pipelines.setdefault(event.run_id, Pipeline([LoopDetector()], dry_run=True))
    pipeline.process(event, event_id=str(event.metadata["event_id"]))
    signal = pipeline.last_decision.signals[0]
    if signal.confidence > 0:
        print(f"LOOP {event.run_id}: {signal.confidence:.2f} {signal.reason}", flush=True)


adapter = HttpIngestAdapter(
    observe, {"penelope": translate}, port=int(os.environ.get("PATHLAYER_PORT", "8787"))
)
adapter.attach()
print(f"Pathlayer écoute http://127.0.0.1:{adapter.port}/ingest", flush=True)
stop = threading.Event()
signal.signal(signal.SIGINT, lambda *_: stop.set())
signal.signal(signal.SIGTERM, lambda *_: stop.set())
stop.wait()
adapter.detach()
