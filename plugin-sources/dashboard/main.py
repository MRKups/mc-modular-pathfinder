#!/usr/bin/env python3
"""A small Python notebook rendered as native Monocode UI.

Code cells run in a separate Python kernel process. The plugin only exchanges
JSON-RPC lines on stdout; kernel output is captured and shown in the notebook.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

SURFACE = "dashboard"
MAX_CELLS = 12
MAX_FILES = 24
MAX_ROWS = 500
SKIP_DIRS = {
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "node_modules",
    "plugins",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".next",
    ".turbo",
    "coverage",
}
DATA_EXTENSIONS = {".csv", ".tsv", ".json"}

KERNEL_SCRIPT = r"""
import ast
import io
import json
import sys
import traceback

namespace = {"__name__": "__main__"}
execution = 0

def describe(value):
    text = repr(value)
    return text[:300] + ("…" if len(text) > 300 else "")

for line in sys.stdin:
    message = json.loads(line)
    code = message.get("code", "")
    execution += 1
    out = io.StringIO()
    err = io.StringIO()
    payload = {"ok": True, "stdout": "", "stderr": "", "result": None, "variables": []}
    try:
        tree = ast.parse(code, "<cell>", "exec")
        last = None
        if tree.body and isinstance(tree.body[-1], ast.Expr):
            last = ast.Expression(tree.body.pop().value)
        with contextlib_redirect(out, err):
            exec(compile(tree, "<cell>", "exec"), namespace)
            value = eval(compile(last, "<cell>", "eval"), namespace) if last else None
        payload["result"] = None if value is None else describe(value)
    except BaseException:
        payload["ok"] = False
        payload["stderr"] = traceback.format_exc()
    payload["stdout"] = out.getvalue()
    payload["stderr"] = err.getvalue() or payload["stderr"]
    variables = []
    for name, value in namespace.items():
        if name.startswith("_") or name == "__name__":
            continue
        if isinstance(value, (type(sys), type, type(lambda: None))):
            continue
        variables.append({"name": name, "type": type(value).__name__, "value": describe(value)})
        if len(variables) >= 60:
            break
    payload["variables"] = variables
    payload["execution"] = execution
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()
"""

# contextlib.redirect_stdout/err are imported separately so the small kernel
# script above stays easy to read.
KERNEL_SCRIPT = KERNEL_SCRIPT.replace(
    "import ast\nimport io\nimport json\nimport sys\nimport traceback",
    "import ast\nimport contextlib\nimport io\nimport json\nimport sys\nimport traceback\n\n"
    "@contextlib.contextmanager\n"
    "def contextlib_redirect(out, err):\n"
    "    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):\n"
    "        yield\n",
)


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


def clean(value: Any) -> str:
    return str(value).replace("\x00", "")


def text(value: Any, size: str = "body", color: str = "ink") -> Dict[str, Any]:
    return {"kind": "text", "text": clean(value)[:16000], "size": size, "color": color}


def button(label: str, handler: str, disabled: bool = False) -> Dict[str, Any]:
    return {"kind": "button", "label": label, "handler": handler, "disabled": disabled}


def input_node(label: str, value: str, placeholder: str, handler: str) -> Dict[str, Any]:
    return {"kind": "input", "label": label, "value": clean(value)[:4096], "placeholder": placeholder, "handler": handler}


def code_editor(
    label: str,
    value: str,
    language: str,
    height: int,
    handler: str,
    read_only: bool = False,
) -> Dict[str, Any]:
    return {
        "kind": "code_editor",
        "label": label,
        "value": clean(value)[: 64 * 1024],
        "language": language,
        "height": max(120, min(900, round(height))),
        "handler": handler,
        "read_only": read_only,
    }


def table(
    columns: List[str],
    rows: List[List[Any]],
    weights: Optional[List[float]] = None,
) -> Dict[str, Any]:
    clean_columns = [clean(column)[:256] for column in columns[:24]]
    clean_rows = []
    for row in rows[:250]:
        clean_rows.append([clean(cell)[:2048] for cell in row[:24]])
    node: Dict[str, Any] = {"kind": "table", "columns": clean_columns, "rows": clean_rows}
    if weights and len(weights) == len(clean_columns):
        node["weights"] = weights
    return node


def section(title: str, children: List[Dict[str, Any]]) -> Dict[str, Any]:
    return {"kind": "column", "gap": 8, "children": [text(title, "subtitle", "ink"), *children]}


def line_count(value: str) -> int:
    return max(1, len(value.splitlines()))


class Kernel:
    def __init__(self) -> None:
        self.process: Optional[subprocess.Popen[str]] = None
        self.responses: "queue.Queue[Dict[str, Any]]" = queue.Queue()
        self.reader: Optional[threading.Thread] = None
        self.cwd = ""
        self.error = ""

    def _start(self) -> bool:
        if self.process is not None and self.process.poll() is None:
            return True
        self.stop()
        cwd = self.cwd if self.cwd and Path(self.cwd).is_dir() else None
        try:
            self.process = subprocess.Popen(
                [sys.executable, "-u", "-c", KERNEL_SCRIPT],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                bufsize=1,
                cwd=cwd,
            )
        except OSError as error:
            self.error = str(error)
            return False
        self.responses = queue.Queue()
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.error = ""
        return True

    def _read(self) -> None:
        process = self.process
        if process is None or process.stdout is None:
            return
        for line in process.stdout:
            try:
                self.responses.put(json.loads(line))
            except json.JSONDecodeError:
                self.responses.put({"ok": False, "stderr": "Kernel emitted invalid JSON."})
        self.responses.put({"ok": False, "stderr": "Kernel exited."})

    def stop(self) -> None:
        process = self.process
        self.process = None
        if process is None:
            return
        try:
            if process.stdin:
                process.stdin.close()
        except OSError:
            pass
        try:
            process.terminate()
            process.wait(timeout=1)
        except Exception:
            try:
                process.kill()
            except Exception:
                pass

    def set_project(self, root: str) -> None:
        if root and root != self.cwd:
            self.cwd = root
            self.stop()

    def run(self, code: str) -> Dict[str, Any]:
        if not self._start():
            return {"ok": False, "stderr": self.error or "Kernel failed to start."}
        process = self.process
        if process is None or process.stdin is None:
            return {"ok": False, "stderr": "Kernel is unavailable."}
        try:
            process.stdin.write(json.dumps({"code": code}) + "\n")
            process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            self.stop()
            return {"ok": False, "stderr": f"Kernel write failed: {error}"}
        try:
            return self.responses.get(timeout=30)
        except queue.Empty:
            self.stop()
            return {"ok": False, "stderr": "Kernel timed out after 30 seconds."}

    def info(self) -> List[List[str]]:
        alive = self.process is not None and self.process.poll() is None
        return [
            ["Python", sys.version.split()[0]],
            ["Executable", sys.executable],
            ["Working directory", self.cwd or "plugin directory"],
            ["Status", "running" if alive else "stopped"],
        ]


class Notebook:
    def __init__(self) -> None:
        self.root: Optional[str] = None
        self.tab = 0
        self.next_cell = 2
        self.execution = 0
        self.cells: List[Dict[str, Any]] = [
            {
                "id": 1,
                "source": "import csv, json, statistics\nfrom pathlib import Path\n\nrows = []\nprint('Python notebook ready')",
                "output": "",
                "ok": True,
                "execution": 0,
            }
        ]
        self.files: List[Dict[str, Any]] = []
        self.path_input = ""
        self.status = ""
        self.variables: List[Dict[str, str]] = []
        self.kernel = Kernel()

    def publish(self) -> None:
        notify("ui.set", {"surface": SURFACE, "tree": self.tree()})

    def scan_files(self) -> None:
        self.files = []
        if not self.root:
            return
        root = Path(self.root)
        for directory, dirs, names in os.walk(root):
            dirs[:] = [name for name in dirs if name not in SKIP_DIRS and not name.startswith(".")]
            depth = len(Path(directory).relative_to(root).parts)
            if depth >= 4:
                dirs[:] = []
            for name in names:
                if len(self.files) >= MAX_FILES:
                    return
                suffix = Path(name).suffix.lower()
                if suffix not in DATA_EXTENSIONS:
                    continue
                path = Path(directory) / name
                try:
                    size = path.stat().st_size
                except OSError:
                    continue
                if size > 20 * 1024 * 1024:
                    continue
                self.files.append(
                    {
                        "path": str(path.relative_to(root)),
                        "kind": suffix.lstrip(".").upper(),
                        "size": size,
                    }
                )
        self.files.sort(key=lambda item: item["path"])

    def format_size(self, size: int) -> str:
        if size >= 1024 * 1024:
            return f"{size / (1024 * 1024):.1f} MB"
        if size >= 1024:
            return f"{size / 1024:.1f} KB"
        return f"{size} B"

    def cell(self, cell_id: int) -> Optional[Dict[str, Any]]:
        return next((cell for cell in self.cells if cell["id"] == cell_id), None)

    def add_cell(self, source: str = "") -> Dict[str, Any]:
        cell = {
            "id": self.next_cell,
            "source": source,
            "output": "",
            "ok": True,
            "execution": 0,
        }
        self.next_cell += 1
        self.cells.append(cell)
        if len(self.cells) > MAX_CELLS:
            self.cells = self.cells[-MAX_CELLS:]
        return cell

    def run_cell(self, cell: Dict[str, Any]) -> None:
        result = self.kernel.run(cell["source"])
        self.execution += 1
        cell["execution"] = self.execution
        stdout = clean(result.get("stdout", ""))
        stderr = clean(result.get("stderr", ""))
        value = clean(result.get("result") or "")
        parts = [part for part in (stdout, stderr) if part]
        if value:
            parts.append(f"Out[{self.execution}]: {value}")
        cell["output"] = "\n".join(parts).strip()
        cell["ok"] = bool(result.get("ok"))
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
        self.status = f"Cell {cell['id']} finished" if cell["ok"] else f"Cell {cell['id']} failed"

    def cell_nodes(self, cell: Dict[str, Any]) -> List[Dict[str, Any]]:
        execution = f"In [{cell['execution']}]" if cell.get("execution") else "In [ ]"
        header = {
            "kind": "row",
            "gap": 8,
            "children": [
                text(execution, "caption", "faint"),
                button("Run", f"run:{cell['id']}"),
                button("Delete", f"delete:{cell['id']}", len(self.cells) <= 1),
            ],
        }
        nodes: List[Dict[str, Any]] = [
            header,
            code_editor(
                "Code",
                cell["source"],
                "py",
                min(320, max(140, line_count(cell["source"]) * 19 + 24)),
                f"cell:{cell['id']}",
            ),
        ]
        if cell["output"]:
            nodes.append(
                code_editor(
                    "Output",
                    cell["output"],
                    "txt",
                    min(420, max(120, line_count(cell["output"]) * 18 + 24)),
                    f"output:{cell['id']}",
                    True,
                )
            )
        return nodes

    def notebook_page(self) -> Dict[str, Any]:
        children: List[Dict[str, Any]] = [
            {
                "kind": "row",
                "gap": 8,
                "children": [
                    button("Run all", "run_all"),
                    button("Clear outputs", "clear_outputs"),
                    button("Add cell", "add_cell"),
                ],
            }
        ]
        for cell in self.cells:
            children.append({"kind": "column", "gap": 8, "children": self.cell_nodes(cell)})
        if self.status:
            children.append(text(self.status, "caption", "muted"))
        return {"kind": "column", "gap": 16, "children": children}

    def data_page(self) -> Dict[str, Any]:
        rows = [[item["path"], item["kind"], self.format_size(item["size"])] for item in self.files]
        children: List[Dict[str, Any]] = [
            {
                "kind": "row",
                "gap": 8,
                "children": [
                    button("Rescan files", "rescan_files"),
                ],
            },
            input_node("File path", self.path_input, "data/example.csv", "path"),
            {
                "kind": "row",
                "gap": 8,
                "children": [button("Insert load cell", "insert_load")],
            },
            section(
                "Data files",
                [table(["Path", "Kind", "Size"], rows, [3.2, 1.0, 1.0])]
                if rows
                else [text("No CSV, TSV, or JSON files found.", "body", "muted")],
            ),
        ]
        return {"kind": "column", "gap": 16, "children": children}

    def variables_page(self) -> Dict[str, Any]:
        rows = [[item["name"], item["type"], item["value"]] for item in self.variables]
        return {
            "kind": "column",
            "gap": 16,
            "children": [
                section(
                    "Variables",
                    [table(["Name", "Type", "Value"], rows, [1.4, 1.1, 3.5])]
                    if rows
                    else [text("No variables yet. Run a cell first.", "body", "muted")],
                )
            ],
        }

    def kernel_page(self) -> Dict[str, Any]:
        return {
            "kind": "column",
            "gap": 16,
            "children": [
                {
                    "kind": "row",
                    "gap": 8,
                    "children": [button("Restart kernel", "restart_kernel")],
                },
                section("Kernel", [table(["Field", "Value"], self.kernel.info(), [1.0, 3.0])]),
                section(
                    "Notebook",
                    [
                        table(
                            ["Cells", "Executions", "Variables"],
                            [[str(len(self.cells)), str(self.execution), str(len(self.variables))]],
                            [1.0, 1.0, 1.0],
                        )
                    ],
                ),
            ],
        }

    def tree(self) -> Dict[str, Any]:
        if self.tab == 1:
            body = self.data_page()
        elif self.tab == 2:
            body = self.variables_page()
        elif self.tab == 3:
            body = self.kernel_page()
        else:
            body = self.notebook_page()
        return {
            "kind": "page_tabs",
            "labels": ["Notebook", "Data", "Variables", "Kernel"],
            "selected": self.tab,
            "handler": "tab",
            "nav_width": 180,
            "children": [body],
        }

    def insert_load_cell(self) -> None:
        path = self.path_input.strip()
        if not path:
            self.status = "Enter a file path first."
            return
        suffix = Path(path).suffix.lower()
        if suffix in (".csv", ".tsv"):
            script = (
                "import csv\n"
                "from pathlib import Path\n\n"
                f"path = Path({json.dumps(path)})\n"
                "with path.open(newline='') as handle:\n"
                "    data = list(csv.DictReader(handle))\n"
                "print(len(data), 'rows')\n"
                "print(list(data[0]) if data else [])"
            )
        elif suffix == ".json":
            script = (
                "import json\n"
                "from pathlib import Path\n\n"
                f"path = Path({json.dumps(path)})\n"
                "with path.open() as handle:\n"
                "    data = json.load(handle)\n"
                "print(type(data).__name__, len(data) if hasattr(data, '__len__') else '')"
            )
        else:
            self.status = "Use a CSV, TSV, or JSON path."
            return
        self.add_cell(script)
        self.status = f"Added loader for {path}"
        self.tab = 0

    def handle_event(self, handler: str, value: Any) -> None:
        if handler == "tab":
            self.tab = max(0, min(3, int(value or 0)))
            self.status = ""
            return
        if handler == "path":
            self.path_input = clean(value)[:512]
            return
        if handler == "rescan_files":
            self.scan_files()
            self.status = f"{len(self.files)} data file(s)"
            return
        if handler == "insert_load":
            self.insert_load_cell()
            return
        if handler == "add_cell":
            self.add_cell("")
            self.status = ""
            return
        if handler == "run_all":
            for cell in list(self.cells):
                self.run_cell(cell)
            return
        if handler == "clear_outputs":
            for cell in self.cells:
                cell["output"] = ""
                cell["ok"] = True
            self.status = ""
            return
        if handler == "restart_kernel":
            self.kernel.stop()
            self.variables = []
            self.execution = 0
            self.status = "Kernel restarted"
            return
        if handler.startswith("run:"):
            cell = self.cell(int(handler.split(":", 1)[1]))
            if cell:
                self.run_cell(cell)
            return
        if handler.startswith("delete:"):
            cell_id = int(handler.split(":", 1)[1])
            if len(self.cells) > 1:
                self.cells = [cell for cell in self.cells if cell["id"] != cell_id]
            return
        if handler.startswith("cell:"):
            cell = self.cell(int(handler.split(":", 1)[1]))
            if cell:
                cell["source"] = clean(value)[: 64 * 1024]
            return
        if handler.startswith("output:"):
            return


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


def main() -> None:
    incoming: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    threading.Thread(target=read_stdin, args=(incoming,), daemon=True).start()
    state = Notebook()

    while True:
        message = incoming.get()
        method = message.get("method")
        if method is None:
            if message.get("id") == 2:
                result = message.get("result") or {}
                root = result.get("root")
                state.root = str(root) if root else None
                if state.root:
                    state.kernel.set_project(state.root)
                state.scan_files()
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
            send({"jsonrpc": "2.0", "id": 2, "method": "workspace.info", "params": {}})
        elif method == "ui.open":
            state.publish()
            send({"jsonrpc": "2.0", "id": 2, "method": "workspace.info", "params": {}})
        elif method == "ui.event":
            params = message.get("params") or {}
            if params.get("surface") != SURFACE:
                continue
            state.handle_event(str(params.get("handler") or ""), params.get("value"))
            state.publish()
        elif method == "shutdown":
            state.kernel.stop()
            break


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        raise SystemExit(0)
