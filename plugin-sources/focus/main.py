#!/usr/bin/env python3
"""A Pomodoro timer that lives in a Python plugin process.

The process owns the clock and session state, speaks newline-delimited JSON-RPC
on stdin/stdout, and publishes a declarative native UI tree. stdout is protocol
traffic only; diagnostics go to stderr.
"""

from __future__ import annotations

import json
import math
import queue
import sys
import threading
import time
from typing import Any, Dict, Optional

SURFACE = "focus"
FOCUS_MINUTES = (25, 45, 60)
BREAK_MINUTES = 5
LOAD_REQUEST = 1
FIRST_SAVE_REQUEST = 2


def send(message: Dict[str, Any]) -> None:
    try:
        sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
        sys.stdout.flush()
    except BrokenPipeError:
        raise SystemExit(0)


def respond(request_id: Any, result: Any) -> None:
    send({"jsonrpc": "2.0", "id": request_id, "result": result})


def notify(method: str, params: Dict[str, Any]) -> None:
    send({"jsonrpc": "2.0", "method": method, "params": params})


def send_error(request_id: Any, code: int, message: str) -> None:
    send(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


class Focus:
    def __init__(self) -> None:
        self.mode = "focus"
        self.focus_choice = 0
        self.running = False
        self.remaining = self.duration()
        self.last_tick = time.monotonic()
        self.completed = 0
        self.dirty = True
        self.load_requested = False
        self.load_request: Optional[int] = None
        self.next_request = FIRST_SAVE_REQUEST

    def duration(self) -> float:
        minutes = (
            FOCUS_MINUTES[self.focus_choice]
            if self.mode == "focus"
            else BREAK_MINUTES
        )
        return float(minutes * 60)

    def seconds_left(self) -> int:
        return max(0, int(math.ceil(self.remaining - 1e-9)))

    def clock(self) -> str:
        seconds = self.seconds_left()
        return f"{seconds // 60:02d}:{seconds % 60:02d}"

    def progress(self) -> float:
        total = self.duration()
        if total <= 0:
            return 0.0
        elapsed = max(0.0, min(1.0, 1.0 - self.remaining / total))
        return round(elapsed * 100.0, 1)

    def advance(self) -> bool:
        if not self.running:
            return False
        now = time.monotonic()
        delta = max(0.0, now - self.last_tick)
        self.last_tick = now
        if delta == 0:
            return False
        before = self.seconds_left()
        self.remaining = max(0.0, self.remaining - delta)
        if self.remaining <= 0:
            self.finish()
            return True
        return self.seconds_left() != before

    def finish(self) -> None:
        if self.mode == "focus":
            self.completed += 1
            self.save()
        self.mode = "break" if self.mode == "focus" else "focus"
        self.running = False
        self.remaining = self.duration()
        self.last_tick = time.monotonic()
        self.dirty = True

    def save(self) -> None:
        request_id = self.next_request
        self.next_request += 1
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "storage.set",
                "params": {
                    "key": "sessions",
                    "value": {"completed": self.completed},
                },
            }
        )

    def request_saved_sessions(self) -> None:
        if self.load_requested:
            return
        self.load_requested = True
        self.load_request = LOAD_REQUEST
        send(
            {
                "jsonrpc": "2.0",
                "id": LOAD_REQUEST,
                "method": "storage.get",
                "params": {"key": "sessions"},
            }
        )

    def consume_response(self, message: Dict[str, Any]) -> None:
        if message.get("id") != self.load_request:
            return
        self.load_request = None
        result = message.get("result")
        if not isinstance(result, dict):
            return
        completed = result.get("completed")
        if isinstance(completed, int) and not isinstance(completed, bool):
            self.completed = max(self.completed, max(0, completed))
            self.dirty = True

    def toggle(self) -> None:
        if self.running:
            self.advance()
            self.running = False
        else:
            if self.remaining <= 0:
                self.remaining = self.duration()
            self.running = True
            self.last_tick = time.monotonic()
        self.dirty = True

    def reset(self) -> None:
        self.running = False
        self.remaining = self.duration()
        self.last_tick = time.monotonic()
        self.dirty = True

    def skip(self) -> None:
        self.running = False
        self.mode = "break" if self.mode == "focus" else "focus"
        self.remaining = self.duration()
        self.last_tick = time.monotonic()
        self.dirty = True

    def change_mode(self, index: int) -> None:
        if index not in (0, 1):
            return
        mode = "focus" if index == 0 else "break"
        if mode == self.mode:
            return
        self.mode = mode
        self.running = False
        self.remaining = self.duration()
        self.last_tick = time.monotonic()
        self.dirty = True

    def change_duration(self, index: int) -> None:
        if not 0 <= index < len(FOCUS_MINUTES):
            return
        self.focus_choice = index
        if self.mode == "focus":
            self.running = False
            self.remaining = self.duration()
            self.last_tick = time.monotonic()
        self.dirty = True

    def handle_event(self, params: Dict[str, Any]) -> None:
        handler = params.get("handler")
        value = params.get("value")
        if handler == "toggle":
            self.toggle()
        elif handler == "reset":
            self.reset()
        elif handler == "skip":
            self.skip()
        elif handler == "mode" and isinstance(value, (int, float)):
            if not isinstance(value, bool):
                self.change_mode(int(value))
        elif handler == "duration" and isinstance(value, (int, float)):
            if not isinstance(value, bool):
                self.change_duration(int(value))

    def tree(self) -> Dict[str, Any]:
        mode_name = "Focus" if self.mode == "focus" else "Break"
        mode_minutes = (
            FOCUS_MINUTES[self.focus_choice]
            if self.mode == "focus"
            else BREAK_MINUTES
        )
        start_label = "Pause" if self.running else "Start"
        reset_disabled = not self.running and self.remaining >= self.duration() - 0.5
        return {
            "kind": "column",
            "gap": 16,
            "children": [
                {"kind": "text", "text": "Focus", "size": "hero", "color": "ink"},
                {
                    "kind": "text",
                    "text": (
                        "A Pomodoro timer owned by a Python process. The clock, "
                        "mode changes, and completed count all live here."
                    ),
                    "size": "body",
                    "color": "muted",
                },
                {
                    "kind": "grid",
                    "columns": 3,
                    "gap": 12,
                    "children": [
                        {
                            "kind": "stat",
                            "label": "Remaining",
                            "value": self.clock(),
                            "detail": "running" if self.running else "paused",
                            "color": "accent",
                        },
                        {
                            "kind": "stat",
                            "label": "Mode",
                            "value": mode_name,
                            "detail": f"{mode_minutes} minute block",
                            "color": "positive" if self.mode == "break" else "accent",
                        },
                        {
                            "kind": "stat",
                            "label": "Completed",
                            "value": str(self.completed),
                            "detail": "focus sessions",
                            "color": "ink",
                        },
                    ],
                },
                {
                    "kind": "card",
                    "gap": 12,
                    "children": [
                        {
                            "kind": "text",
                            "text": "Current session",
                            "size": "subtitle",
                            "color": "ink",
                        },
                        {
                            "kind": "gauge",
                            "label": "Progress",
                            "value": self.progress(),
                            "min": 0,
                            "max": 100,
                            "unit": "%",
                            "color": "accent",
                        },
                        {
                            "kind": "row",
                            "gap": 8,
                            "children": [
                                {
                                    "kind": "button",
                                    "label": start_label,
                                    "handler": "toggle",
                                    "disabled": False,
                                },
                                {
                                    "kind": "button",
                                    "label": "Reset",
                                    "handler": "reset",
                                    "disabled": reset_disabled,
                                },
                                {
                                    "kind": "button",
                                    "label": "Skip",
                                    "handler": "skip",
                                    "disabled": False,
                                },
                            ],
                        },
                        {
                            "kind": "text",
                            "text": (
                                f"{self.clock()} left in this "
                                f"{mode_name.lower()} block."
                            ),
                            "size": "small",
                            "color": "faint",
                        },
                    ],
                },
                {
                    "kind": "card",
                    "gap": 12,
                    "children": [
                        {
                            "kind": "text",
                            "text": "Session settings",
                            "size": "subtitle",
                            "color": "ink",
                        },
                        {
                            "kind": "tabs",
                            "labels": ["Focus", "Break"],
                            "selected": 0 if self.mode == "focus" else 1,
                            "handler": "mode",
                        },
                        {
                            "kind": "select",
                            "label": "Focus length",
                            "options": [
                                f"{minutes} minutes" for minutes in FOCUS_MINUTES
                            ],
                            "selected": self.focus_choice,
                            "handler": "duration",
                        },
                        {
                            "kind": "text",
                            "text": (
                                "Changing the focus length resets a focus block. "
                                "Break blocks are 5 minutes."
                            ),
                            "size": "small",
                            "color": "faint",
                        },
                    ],
                },
                {
                    "kind": "text",
                    "text": (
                        "Completed sessions are saved through the host's "
                        "plugin-scoped storage when a data directory is available."
                    ),
                    "size": "small",
                    "color": "muted",
                },
            ],
        }

    def publish(self) -> None:
        notify("ui.set", {"surface": SURFACE, "tree": self.tree()})
        self.dirty = False

    def handle(self, message: Dict[str, Any]) -> bool:
        method = message.get("method")
        request_id = message.get("id")

        if method is None:
            self.consume_response(message)
            return True

        if method == "initialize":
            respond(
                request_id,
                {
                    "protocol": 1,
                    "ui": [
                        {
                            "id": SURFACE,
                            "title": "Focus",
                            "icon": "clock",
                            "order": 4,
                        }
                    ],
                },
            )
            self.dirty = True
        elif method == "ui.open":
            self.request_saved_sessions()
            self.dirty = True
        elif method == "ui.close":
            pass
        elif method == "ui.event":
            params = message.get("params")
            if isinstance(params, dict) and params.get("surface") == SURFACE:
                self.handle_event(params)
        elif method == "shutdown":
            return False
        elif request_id is not None:
            send_error(request_id, -32601, f"Unknown method: {method}")
        return True


def read_stdin(inbox: "queue.Queue[Optional[Dict[str, Any]]]") -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            print(f"focus plugin: invalid JSON: {error}", file=sys.stderr)
            continue
        if isinstance(message, dict):
            inbox.put(message)
    inbox.put(None)


def main() -> None:
    inbox: "queue.Queue[Optional[Dict[str, Any]]]" = queue.Queue()
    threading.Thread(target=read_stdin, args=(inbox,), daemon=True).start()
    plugin = Focus()

    while True:
        try:
            message = inbox.get(timeout=0.2)
        except queue.Empty:
            if plugin.advance():
                plugin.publish()
            continue

        if message is None:
            break
        if not plugin.handle(message):
            break
        if plugin.advance():
            plugin.dirty = True
        if plugin.dirty:
            plugin.publish()


if __name__ == "__main__":
    main()
