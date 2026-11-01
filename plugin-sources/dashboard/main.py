#!/usr/bin/env python3
"""Python notebook workbench rendered with native Monocode UI components.

Three-column layout:
1. Left column: live session variables table.
2. Center column: continuous Python code sheet with selection + right-click run.
3. Right column: run controls and structured outputs (stdout, tables, charts).

Code executes in a persistent child Python process rooted in the active
workspace. Plugin stdout carries JSON-RPC 2.0 protocol lines only.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Dict, List, Optional

SURFACE = "dashboard"

STARTER_CODE = (
    "# 1. Scan workspace text files\n"
    "import os\n"
    "from collections import defaultdict\n"
    "from pathlib import Path\n\n"
    "skip = {'.git', 'target', 'node_modules', 'plugins', 'dist', 'build', '.venv'}\n"
    "records = []\n"
    "for root, dirs, names in os.walk('.'):\n"
    "    dirs[:] = sorted(d for d in dirs if d not in skip)\n"
    "    for name in sorted(names):\n"
    "        if len(records) >= 2500:\n"
    "            break\n"
    "        path = Path(root) / name\n"
    "        if not path.is_file() or path.stat().st_size > 2_000_000:\n"
    "            continue\n"
    "        raw = path.read_bytes()\n"
    "        if b'\\x00' in raw[:512]:\n"
    "            continue\n"
    "        records.append({\n"
    "            'path': os.path.relpath(path, '.'),\n"
    "            'ext': path.suffix.lower() or name,\n"
    "            'lines': raw.count(b'\\n') + (1 if raw else 0),\n"
    "            'kb': round(len(raw) / 1024, 1),\n"
    "        })\n\n"
    "total_lines = sum(r['lines'] for r in records)\n"
    "print(f\"{len(records)} files, {total_lines:,} lines\")\n\n"
    "# 2. Summarize lines and share by extension\n"
    "by_ext = defaultdict(lambda: {'files': 0, 'lines': 0, 'kb': 0.0})\n"
    "for item in records:\n"
    "    bucket = by_ext[item['ext']]\n"
    "    bucket['files'] += 1\n"
    "    bucket['lines'] += item['lines']\n"
    "    bucket['kb'] += item['kb']\n\n"
    "summary = [\n"
    "    {\n"
    "        'ext': ext,\n"
    "        'files': stats['files'],\n"
    "        'lines': stats['lines'],\n"
    "        'share': f\"{(stats['lines'] / max(1, total_lines)) * 100:.1f}%\",\n"
    "    }\n"
    "    for ext, stats in sorted(by_ext.items(), key=lambda kv: kv[1]['lines'], reverse=True)[:8]\n"
    "]\n"
    "summary\n\n"
    "# 3. Plot top file types (highlight and right-click -> Run selected)\n"
    "labels = [row['ext'] for row in summary]\n"
    "line_counts = [row['lines'] for row in summary]\n"
    "file_counts = [row['files'] for row in summary]\n"
    "plot(line_counts, labels=labels, kind='bar', label='lines by type')\n\n"
    "# 4. Top 10 largest files\n"
    "largest = sorted(records, key=lambda r: r['lines'], reverse=True)[:10]\n"
    "[\n"
    "    {'rank': i + 1, 'path': r['path'], 'lines': r['lines'], 'kb': r['kb']}\n"
    "    for i, r in enumerate(largest)\n"
    "]"
)

KERNEL_SCRIPT = r"""
import ast
import contextlib
import io
import json
import math
import sys
import textwrap
import time
import traceback

_EMITTED = []


class _VisualOutput:
    def __init__(self, item):
        self.item = item

    def __repr__(self):
        kind = self.item.get("kind", "visual")
        label = self.item.get("label", "")
        return f"<{kind}: {label}>" if label else f"<{kind}>"


def short_repr(value, limit=200):
    try:
        text = repr(value)
    except Exception as exc:
        text = f"<repr error: {exc}>"
    text = text.replace("\x00", "")
    if len(text) > limit:
        return text[: limit - 3] + "..."
    return text


def cell_str(value, limit=180):
    if value is None:
        return ""
    if isinstance(value, (str, int, float, bool)):
        text = str(value)
    else:
        text = short_repr(value, limit)
    text = text.replace("\x00", "").replace("\r", " ").replace("\n", " ")
    if len(text) > limit:
        return text[: limit - 3] + "..."
    return text


def serialize_value(value):
    if value is None or isinstance(value, _VisualOutput):
        return None
    if isinstance(value, (list, tuple)) and value:
        if all(isinstance(row, dict) for row in value):
            cols = []
            for row in value[:50]:
                for key in row.keys():
                    name = cell_str(key, 64)
                    if name not in cols and len(cols) < 10:
                        cols.append(name)
            if cols:
                rows = [
                    [cell_str(row.get(col, "")) for col in cols]
                    for row in value[:40]
                ]
                return {"kind": "table", "columns": cols, "rows": rows, "total": len(value)}
        first = value[0]
        if (
            isinstance(first, (list, tuple))
            and 2 <= len(first) <= 10
            and all(isinstance(row, (list, tuple)) and len(row) == len(first) for row in value[:40])
        ):
            cols = [f"c{i}" for i in range(len(first))]
            rows = [[cell_str(item) for item in row] for row in value[:40]]
            return {"kind": "table", "columns": cols, "rows": rows, "total": len(value)}
    if isinstance(value, dict) and value and len(value) <= 40:
        rows = [[cell_str(k, 80), cell_str(v, 180)] for k, v in list(value.items())[:40]]
        return {"kind": "table", "columns": ["Key", "Value"], "rows": rows, "total": len(value)}
    return {"kind": "scalar", "text": short_repr(value, 600)}


def plot(values, labels=None, kind="bar", label="value"):
    chart_kind = str(kind).lower()
    if chart_kind not in ("bar", "line", "area"):
        raise ValueError("plot kind must be 'bar', 'line', or 'area'")
    nums = [float(v) for v in list(values)[:128]]
    if not nums or any(not math.isfinite(v) for v in nums):
        raise ValueError("plot requires 1..128 finite numeric values")
    out_labels = [cell_str(x, 48) for x in list(labels)[: len(nums)]] if labels is not None else []
    if out_labels and len(out_labels) != len(nums):
        raise ValueError("plot labels length must match values")
    item = {
        "kind": "chart",
        "chart_type": chart_kind,
        "label": cell_str(label, 64) or "value",
        "values": nums,
        "labels": out_labels,
    }
    if len(_EMITTED) < 6:
        _EMITTED.append(item)
    return _VisualOutput(item)


def show(value):
    item = serialize_value(value)
    if item is not None and len(_EMITTED) < 6:
        _EMITTED.append(item)
    return _VisualOutput(item or {"kind": "scalar"})


def type_with_size(value):
    tname = type(value).__name__
    if isinstance(value, (bool, int, float)):
        return tname
    if isinstance(value, (str, bytes, list, tuple, set, dict)):
        try:
            return f"{tname}[{len(value)}]"
        except Exception:
            return tname
    shape = getattr(value, "shape", None)
    if isinstance(shape, tuple) and shape:
        dims = "x".join(str(d) for d in shape[:4])
        return f"{tname}[{dims}]"
    return tname


def snapshot_variables(ns):
    hidden = {"__builtins__", "plot", "show"}
    rows = []
    for name in sorted(ns.keys()):
        if name in hidden or name.startswith("_"):
            continue
        val = ns[name]
        if isinstance(val, type(sys)) or callable(val) or isinstance(val, type):
            continue
        rows.append(
            {
                "name": name[:64],
                "type": type_with_size(val)[:32],
                "value": cell_str(val, 140),
            }
        )
        if len(rows) >= 120:
            break
    return rows


def format_Failure(exc, source, line_offset=0):
    lines = source.splitlines()
    if isinstance(exc, SyntaxError):
        lineno = exc.lineno or 1
        sheet_line = lineno + line_offset
        src_line = lines[lineno - 1].strip() if 1 <= lineno <= len(lines) else ""
        msg = f"L{sheet_line:03d}  SyntaxError: {exc.msg}"
        if src_line:
            msg += f"\n      {src_line}"
        return msg
    tb = exc.__traceback__
    cell_lines = []
    while tb is not None:
        frame = tb.tb_frame
        if frame.f_code.co_filename == "<notebook>":
            cell_lines.append(tb.tb_lineno)
        tb = tb.tb_next
    headline = f"{type(exc).__name__}: {exc}"
    if cell_lines:
        lineno = cell_lines[-1]
        sheet_line = lineno + line_offset
        src_line = lines[lineno - 1].strip() if 1 <= lineno <= len(lines) else ""
        out = f"L{sheet_line:03d}  {headline}"
        if src_line:
            out += f"\n      {src_line}"
        return out
    formatted = traceback.format_exception_only(type(exc), exc)
    return "".join(formatted).strip()


def prepare_source(raw):
    dedented = textwrap.dedent(raw)
    try:
        return dedented, ast.parse(dedented, "<notebook>", "exec")
    except IndentationError:
        if "\n" in raw:
            first, rest = raw.split("\n", 1)
            if first and not first[0].isspace() and not first.rstrip().endswith(":"):
                candidate = first + "\n" + textwrap.dedent(rest)
                return candidate, ast.parse(candidate, "<notebook>", "exec")
        raise


namespace = {"__name__": "__main__", "plot": plot, "show": show}

for raw_line in sys.stdin:
    try:
        request = json.loads(raw_line)
    except Exception as exc:
        sys.stdout.write(json.dumps({"ok": False, "error": str(exc)}) + "\n")
        sys.stdout.flush()
        continue

    if request.get("action") == "inspect":
        target_name = str(request.get("name") or "")
        items = []
        ok = True
        err = ""
        if target_name in namespace:
            item = serialize_value(namespace[target_name])
            if item is not None:
                items.append(item)
        else:
            ok = False
            err = f"Variable '{target_name}' is not defined."
        sys.stdout.write(
            json.dumps(
                {
                    "ok": ok,
                    "stdout": "",
                    "error": err,
                    "outputs": items,
                    "variables": snapshot_variables(namespace),
                    "ms": 0,
                }
            )
            + "\n"
        )
        sys.stdout.flush()
        continue

    source = request.get("code", "")
    line_offset = int(request.get("line_offset") or 0)
    _EMITTED.clear()
    stdout_buf = io.StringIO()
    stderr_buf = io.StringIO()
    ok = True
    error_text = ""
    outputs = []
    started = time.perf_counter()

    try:
        with contextlib.redirect_stdout(stdout_buf), contextlib.redirect_stderr(stderr_buf):
            if source.strip():
                prepared, parsed = prepare_source(source)
                body = list(parsed.body)
                tail_expr = None
                if body and isinstance(body[-1], ast.Expr):
                    tail_expr = ast.Expression(body.pop().value)
                if body:
                    exec_mod = ast.Module(body=body, type_ignores=[])
                    ast.fix_missing_locations(exec_mod)
                    exec(compile(exec_mod, "<notebook>", "exec"), namespace)
                if tail_expr is not None:
                    ast.fix_missing_locations(tail_expr)
                    last_val = eval(compile(tail_expr, "<notebook>", "eval"), namespace)
                    namespace["_"] = last_val
                    tail_item = serialize_value(last_val)
                    if tail_item is not None:
                        _EMITTED.append(tail_item)
                outputs = list(_EMITTED)
    except BaseException as exc:
        ok = False
        outputs = list(_EMITTED)
        error_text = format_Failure(exc, source, line_offset)

    elapsed_ms = max(1, int(round((time.perf_counter() - started) * 1000)))
    out_text = stdout_buf.getvalue()
    err_stream = stderr_buf.getvalue()
    if err_stream:
        out_text = (out_text + ("\n" if out_text and not out_text.endswith("\n") else "") + err_stream)

    response = {
        "ok": ok,
        "stdout": out_text[-12000:],
        "error": error_text[-6000:],
        "outputs": outputs,
        "variables": snapshot_variables(namespace),
        "ms": elapsed_ms,
    }
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"""

WRITE_LOCK = threading.Lock()


def send(message: Dict[str, Any]) -> None:
    with WRITE_LOCK:
        sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def respond(request_id: Any, result: Any) -> None:
    send({"jsonrpc": "2.0", "id": request_id, "result": result})


def notify(method: str, params: Dict[str, Any]) -> None:
    send({"jsonrpc": "2.0", "method": method, "params": params})


def clean(value: Any) -> str:
    return str(value if value is not None else "").replace("\x00", "").replace("\u2014", "-")


def text(
    value: str,
    size: str = "body",
    color: str = "ink",
    monospace: bool = False,
) -> Dict[str, Any]:
    node: Dict[str, Any] = {
        "kind": "text",
        "text": clean(value)[:16000],
        "size": size,
        "color": color,
    }
    if monospace:
        node["monospace"] = True
    return node


def button(label: str, handler: str, disabled: bool = False) -> Dict[str, Any]:
    return {
        "kind": "button",
        "label": clean(label)[:128],
        "handler": handler,
        "disabled": bool(disabled),
    }


def spacer() -> Dict[str, Any]:
    return {"kind": "spacer"}


def column(
    children: List[Dict[str, Any]],
    gap: int = 10,
    weight: Optional[float] = None,
) -> Dict[str, Any]:
    node: Dict[str, Any] = {"kind": "column", "gap": gap, "children": children}
    if weight is not None:
        node["weight"] = float(weight)
    return node


def row(children: List[Dict[str, Any]], gap: int = 8) -> Dict[str, Any]:
    return {"kind": "row", "gap": gap, "children": children}


def table(
    columns: List[str],
    rows: List[List[Any]],
    weights: Optional[List[float]] = None,
    handler: Optional[str] = None,
    selected: Optional[int] = None,
) -> Dict[str, Any]:
    clean_columns = [clean(col)[:256] for col in columns[:24]]
    width = len(clean_columns)
    clean_rows = []
    for item in rows[:250]:
        padded = list(item[:width]) + [""] * max(0, width - len(item))
        clean_rows.append([clean(cell)[:2048] for cell in padded])
    node: Dict[str, Any] = {"kind": "table", "columns": clean_columns, "rows": clean_rows}
    if weights and len(weights) == width:
        node["weights"] = [max(0.1, min(20.0, float(w))) for w in weights]
    if handler:
        node["handler"] = handler
    if selected is not None and 0 <= selected < len(clean_rows):
        node["selected"] = selected
    return node


def sheet_height(source: str) -> int:
    lines = max(1, len(source.splitlines()) + (1 if source.endswith("\n") else 0))
    return min(1280, max(740, (lines + 2) * 20 + 44))


def selection_line_count(snippet: str) -> int:
    stripped = snippet.strip()
    if not stripped:
        return 0
    return len(stripped.splitlines())


def infer_table_weights(columns: List[str], rows: List[List[str]]) -> List[float]:
    weights: List[float] = []
    sample = rows[:20]
    for idx, col in enumerate(columns):
        lengths = [len(col)] + [len(r[idx]) for r in sample if idx < len(r)]
        avg = sum(lengths) / max(1, len(lengths))
        numeric = sample and all(
            idx < len(r)
            and r[idx].replace(".", "", 1).replace("-", "", 1).replace("%", "", 1).isdigit()
            for r in sample
            if idx < len(r) and r[idx]
        )
        if numeric:
            weights.append(0.8)
        elif avg > 32:
            weights.append(3.0)
        elif avg > 15:
            weights.append(1.9)
        else:
            weights.append(1.1)
    return weights


class Kernel:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.process: Optional[subprocess.Popen[str]] = None
        self.responses: "queue.Queue[Dict[str, Any]]" = queue.Queue()
        self.reader: Optional[threading.Thread] = None
        self.cwd = ""
        self.error = ""

    def _start_locked(self) -> bool:
        if self.process is not None and self.process.poll() is None:
            return True
        self.responses = queue.Queue()
        cwd = self.cwd if self.cwd and os.path.isdir(self.cwd) else os.getcwd()
        env = dict(os.environ)
        env["PYTHONUNBUFFERED"] = "1"
        try:
            proc = subprocess.Popen(
                [sys.executable, "-u", "-c", KERNEL_SCRIPT],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                bufsize=1,
                cwd=cwd,
                env=env,
            )
        except OSError as error:
            self.error = str(error)
            self.process = None
            return False

        self.process = proc
        self.error = ""
        out_queue = self.responses

        def pump_stdout() -> None:
            assert proc.stdout is not None
            for line in proc.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    payload = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(payload, dict):
                    out_queue.put(payload)

        self.reader = threading.Thread(target=pump_stdout, daemon=True)
        self.reader.start()
        return True

    def _stop_locked(self) -> None:
        proc = self.process
        self.process = None
        if proc is None:
            return
        try:
            if proc.stdin:
                proc.stdin.close()
        except Exception:
            pass
        try:
            proc.terminate()
            proc.wait(timeout=0.5)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass

    def stop(self) -> None:
        with self.lock:
            self._stop_locked()

    def set_project(self, root: str) -> bool:
        if root and root != self.cwd:
            self.cwd = root
            self.stop()
            return True
        return False

    def request(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        with self.lock:
            if not self._start_locked():
                return {"ok": False, "error": self.error or "Kernel failed to start."}
            proc = self.process
            responses = self.responses
            if proc is None or proc.stdin is None:
                return {"ok": False, "error": "Kernel unavailable."}
            try:
                proc.stdin.write(json.dumps(payload) + "\n")
                proc.stdin.flush()
            except (BrokenPipeError, OSError) as error:
                self._stop_locked()
                return {"ok": False, "error": f"Kernel write failed: {error}"}
        try:
            return responses.get(timeout=30)
        except queue.Empty:
            self.stop()
            return {"ok": False, "error": "Timed out after 30 s."}


class NotebookWorkbench:
    def __init__(self, incoming: "queue.Queue[Dict[str, Any]]") -> None:
        self.incoming = incoming
        self.next_req_id = 10
        self.pending_requests: Dict[int, str] = {}
        self.root: Optional[str] = None
        self.workspace_name = ""

        self.source = STARTER_CODE
        self.height = sheet_height(STARTER_CODE)
        self.selection = ""

        self.variables: List[Dict[str, str]] = []
        self.selected_var: Optional[int] = None

        self.busy = False
        self.run_mode = "all"
        self.run_generation = 0
        self.execution = 0
        self.ok = True
        self.last_ms: Optional[int] = None
        self.last_label = ""
        self.stdout = ""
        self.error = ""
        self.outputs: List[Dict[str, Any]] = []

        self.kernel = Kernel()

    def request_host(self, method: str, params: Dict[str, Any]) -> None:
        req_id = self.next_req_id
        self.next_req_id += 1
        self.pending_requests[req_id] = method
        send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params})

    def publish(self) -> None:
        notify("ui.set", {"surface": SURFACE, "tree": self.tree()})

    def handle_host_response(self, message: Dict[str, Any]) -> bool:
        req_id = message.get("id")
        if not isinstance(req_id, int):
            return False
        method = self.pending_requests.pop(req_id, None)
        if method != "workspace.info" or "error" in message:
            return False
        result = message.get("result")
        if isinstance(result, dict):
            self.root = str(result["root"]) if result.get("root") else None
            self.workspace_name = clean(result.get("name") or "")
            if self.root and self.kernel.set_project(self.root):
                self.variables = []
                self.selected_var = None
                self.execution = 0
            return True
        return False

    def has_output(self) -> bool:
        return bool(self.stdout or self.error or self.outputs or self.execution > 0)

    def _line_offset_for(self, snippet: str) -> int:
        if not snippet or snippet == self.source:
            return 0
        pos = self.source.find(snippet)
        if pos < 0:
            return 0
        return self.source[:pos].count("\n")

    def start_code_run(self, snippet: Optional[str]) -> bool:
        if self.busy:
            return False
        is_selection = snippet is not None
        code = snippet if snippet is not None else self.source
        if not code.strip():
            return False
        self.busy = True
        self.selected_var = None
        if is_selection:
            lines = selection_line_count(code)
            self.run_mode = f"selected ({lines}L)" if lines > 1 else "selected"
        else:
            self.run_mode = "all"
        line_offset = self._line_offset_for(code) if is_selection else 0
        generation = self.run_generation
        incoming = self.incoming
        kernel = self.kernel

        def worker() -> None:
            result = kernel.request({"code": code, "line_offset": line_offset})
            incoming.put(
                {
                    "_internal": "run_done",
                    "generation": generation,
                    "label": self.run_mode,
                    "count_exec": True,
                    "result": result,
                }
            )

        threading.Thread(target=worker, daemon=True).start()
        return True

    def start_inspect_var(self, index: int) -> bool:
        if self.busy or not (0 <= index < len(self.variables)):
            return False
        name = self.variables[index]["name"]
        self.selected_var = index
        self.busy = True
        self.run_mode = f"var {name}"
        generation = self.run_generation
        incoming = self.incoming
        kernel = self.kernel

        def worker() -> None:
            result = kernel.request({"action": "inspect", "name": name})
            incoming.put(
                {
                    "_internal": "run_done",
                    "generation": generation,
                    "label": f"var {name}",
                    "count_exec": False,
                    "result": result,
                }
            )

        threading.Thread(target=worker, daemon=True).start()
        return True

    def finish_run(
        self,
        generation: int,
        label: str,
        count_exec: bool,
        result: Dict[str, Any],
    ) -> bool:
        if generation != self.run_generation:
            return False
        self.busy = False
        if count_exec:
            self.execution += 1
        self.last_label = label
        self.ok = bool(result.get("ok"))
        self.last_ms = result.get("ms") if count_exec else None
        self.stdout = clean(result.get("stdout", "")).rstrip()
        self.error = clean(result.get("error", "")).strip()
        raw_outputs = result.get("outputs")
        self.outputs = [item for item in raw_outputs if isinstance(item, dict)] if isinstance(raw_outputs, list) else []
        variables = result.get("variables")
        if isinstance(variables, list):
            self.variables = [
                {
                    "name": clean(item.get("name", "")),
                    "type": clean(item.get("type", "")),
                    "value": clean(item.get("value", "")),
                }
                for item in variables
                if isinstance(item, dict)
            ]
            if self.selected_var is not None and self.selected_var >= len(self.variables):
                self.selected_var = None
        return True

    def interrupt(self) -> None:
        if not self.busy:
            return
        self.run_generation += 1
        self.busy = False
        self.kernel.stop()
        self.variables = []
        self.selected_var = None
        self.ok = False
        self.last_ms = None
        self.error = "Interrupted (session restarted)"

    def restart_session(self) -> None:
        if self.busy:
            self.interrupt()
        else:
            self.run_generation += 1
            self.kernel.stop()
            self.variables = []
            self.selected_var = None
        self.execution = 0
        self.last_ms = None
        self.last_label = ""
        self.stdout = ""
        self.error = ""
        self.outputs = []
        self.ok = True

    def clear_output(self) -> None:
        self.stdout = ""
        self.error = ""
        self.outputs = []
        self.error = ""
        self.last_ms = None
        self.last_label = ""
        self.selected_var = None
        self.ok = True

    def render_output_item(self, item: Dict[str, Any]) -> List[Dict[str, Any]]:
        nodes: List[Dict[str, Any]] = []
        kind = item.get("kind")
        if kind == "table":
            cols = item.get("columns") or []
            rows = item.get("rows") or []
            total = int(item.get("total") or len(rows))
            if cols:
                nodes.append(table(cols, rows, infer_table_weights(cols, rows)))
                if total > len(rows):
                    nodes.append(
                        text(f"{len(rows)} of {total} rows", "caption", "faint", monospace=True)
                    )
        elif kind == "chart":
            values = item.get("values") or []
            labels = item.get("labels") or []
            if values:
                nodes.append(
                    {
                        "kind": "chart",
                        "chart_type": item.get("chart_type") or "bar",
                        "series": [
                            {
                                "label": clean(item.get("label") or "value")[:128],
                                "values": values[:128],
                                "color": "accent",
                            }
                        ],
                        "labels": [clean(lbl)[:64] for lbl in labels[: len(values)]],
                        "height": 190,
                    }
                )
        elif kind == "scalar" and item.get("text"):
            nodes.append(text(f"-> {item['text']}", "small", "muted", monospace=True))
        return nodes

    def variables_column(self) -> Dict[str, Any]:
        count = len(self.variables)
        title = f"Variables ({count})" if count else "Variables"
        header = row(
            [
                text(title, "small", "muted", monospace=True),
                spacer(),
                button("Restart", "restart_kernel", not self.busy and count == 0 and self.execution == 0),
            ],
            gap=8,
        )

        workspace = self.workspace_name or (Path(self.root).name if self.root else "workspace")
        py_ver = sys.version.split()[0]
        meta = text(f"py {py_ver} · {workspace}", "caption", "faint", monospace=True)

        children: List[Dict[str, Any]] = [header, meta]
        if self.variables:
            rows = [[v["name"], v["type"], v["value"]] for v in self.variables]
            children.append(
                table(
                    ["Name", "Type", "Value"],
                    rows,
                    [1.1, 0.9, 1.9],
                    handler="pick_var",
                    selected=self.selected_var,
                )
            )
        else:
            children.append(text("No variables in session.", "caption", "muted", monospace=True))
        return column(children, gap=10, weight=0.95)

    def code_column(self) -> Dict[str, Any]:
        total_lines = max(1, len(self.source.splitlines()))
        sel_lines = selection_line_count(self.selection)
        info = f"notebook.py · {total_lines}L"
        if sel_lines > 0:
            info += f" · {sel_lines}L selected"

        header = row(
            [
                text(info, "small", "muted", monospace=True),
                spacer(),
                button("Reset", "reset_sheet", self.busy or self.source == STARTER_CODE),
            ],
            gap=8,
        )

        editor: Dict[str, Any] = {
            "kind": "code_editor",
            "label": "Notebook sheet",
            "value": self.source,
            "language": "py",
            "height": self.height,
            "handler": "edit",
            "submit_handler": "submit_code",
            "selection_handler": "select",
            "context_menu": [
                {
                    "label": "Run selected",
                    "handler": "ctx_run_selected",
                    "disabled": self.busy,
                    "requires_selection": True,
                },
                {
                    "label": "Run all",
                    "handler": "run_all",
                    "disabled": self.busy,
                },
            ],
        }
        return column([header, editor], gap=10, weight=1.80)

    def output_column(self) -> Dict[str, Any]:
        sel_lines = selection_line_count(self.selection)
        run_sel_label = f"Run selected ({sel_lines}L)" if sel_lines > 1 else "Run selected"
        primary_action = (
            button("Stop", "interrupt")
            if self.busy
            else button("Run all", "run_all", not self.source.strip())
        )
        controls = row(
            [
                button(run_sel_label, "run_selected", self.busy or sel_lines == 0),
                primary_action,
                spacer(),
                button("Clear", "clear_output", self.busy or not self.has_output()),
            ],
            gap=6,
        )

        if self.busy:
            status = text(f"[*]  running {self.run_mode}", "caption", "accent", monospace=True)
        elif not self.ok:
            tag = f"[{self.execution:02d}]" if self.execution else "[!]"
            status = text(f"{tag}  error · {self.last_label or 'run'}", "caption", "negative", monospace=True)
        elif self.last_label:
            parts = []
            if self.execution:
                parts.append(f"[{self.execution:02d}]")
            parts.append("pass")
            if self.last_ms is not None:
                parts.append(f"{self.last_ms} ms")
            parts.append(self.last_label)
            status = text(" · ".join(parts), "caption", "muted", monospace=True)
        else:
            status = text("idle · output", "caption", "faint", monospace=True)

        children: List[Dict[str, Any]] = [controls, status]

        if self.stdout:
            out_lines = self.stdout.splitlines()
            shown = "\n".join(out_lines[:100])
            if len(out_lines) > 100:
                shown += f"\n... ({len(out_lines) - 100} more lines)"
            children.append(text(shown, "small", "ink", monospace=True))

        for item in self.outputs[:8]:
            children.extend(self.render_output_item(item))

        if self.error:
            children.append(text(self.error, "small", "negative", monospace=True))

        return column(children, gap=10, weight=1.25)

    def tree(self) -> Dict[str, Any]:
        return row(
            [
                self.variables_column(),
                self.code_column(),
                self.output_column(),
            ],
            gap=18,
        )

    def handle_event(self, handler: str, value: Any) -> bool:
        if handler == "edit":
            updated = clean(value)[: 64 * 1024]
            if updated == self.source:
                return False
            was_starter = self.source == STARTER_CODE
            old_lines = len(self.source.splitlines())
            self.source = updated
            new_height = sheet_height(updated)
            new_lines = len(updated.splitlines())
            height_changed = new_height != self.height
            self.height = new_height
            is_starter = self.source == STARTER_CODE
            return height_changed or new_lines != old_lines or (was_starter != is_starter)

        if handler == "select":
            prev_count = selection_line_count(self.selection)
            updated = clean(value)[: 64 * 1024] if isinstance(value, str) else ""
            self.selection = updated
            return selection_line_count(updated) != prev_count

        if handler == "run_selected":
            if not self.selection.strip():
                return False
            return self.start_code_run(self.selection)

        if handler == "ctx_run_selected":
            snippet = clean(value)[: 64 * 1024] if isinstance(value, str) and value.strip() else self.selection
            if not snippet.strip():
                return False
            self.selection = snippet
            return self.start_code_run(snippet)

        if handler == "submit_code":
            if isinstance(value, str) and value.strip():
                snippet = clean(value)[: 64 * 1024]
                self.selection = snippet
                return self.start_code_run(snippet)
            return self.start_code_run(None)

        if handler == "run_all":
            return self.start_code_run(None)

        if handler == "interrupt":
            self.interrupt()
            return True

        if handler == "clear_output":
            if self.busy:
                return False
            self.clear_output()
            return True

        if handler == "restart_kernel":
            self.restart_session()
            return True

        if handler == "reset_sheet":
            if self.busy:
                return False
            self.source = STARTER_CODE
            self.height = sheet_height(STARTER_CODE)
            self.selection = ""
            return True

        if handler == "pick_var":
            idx = int(value) if isinstance(value, (int, float)) else -1
            return self.start_inspect_var(idx)

        return False


def read_stdin(incoming: "queue.Queue[Dict[str, Any]]") -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(message, dict):
            incoming.put(message)
    incoming.put({"_internal": "eof"})


def main() -> None:
    incoming: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    threading.Thread(target=read_stdin, args=(incoming,), daemon=True).start()
    state = NotebookWorkbench(incoming)

    while True:
        message = incoming.get()
        internal = message.get("_internal")
        if internal == "eof":
            state.kernel.stop()
            break
        if internal == "run_done":
            if state.finish_run(
                int(message["generation"]),
                str(message.get("label") or "run"),
                bool(message.get("count_exec", True)),
                message["result"],
            ):
                state.publish()
            continue

        method = message.get("method")
        if method is None:
            if state.handle_host_response(message):
                state.publish()
            continue

        if method == "initialize":
            respond(
                message.get("id"),
                {
                    "protocol": 1,
                    "ui": [
                        {
                            "id": SURFACE,
                            "title": "Notebook",
                            "icon": "note",
                            "order": 1,
                        }
                    ],
                },
            )
            state.publish()
            state.request_host("workspace.info", {})
        elif method == "ui.open":
            state.publish()
            state.request_host("workspace.info", {})
        elif method == "ui.event":
            params = message.get("params") or {}
            if params.get("surface") != SURFACE:
                continue
            if state.handle_event(str(params.get("handler") or ""), params.get("value")):
                state.publish()
        elif method == "shutdown":
            state.kernel.stop()
            break


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        raise SystemExit(0)
