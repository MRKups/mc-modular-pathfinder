#!/usr/bin/env python3
"""A workspace data workbench rendered as native Monocode UI.

The plugin discovers CSV/TSV/JSON files under the active project, shapes them
into charts and tables, and republishes its tree whenever the user changes a
control or a live file changes on disk. stdout is protocol traffic only.
"""

from __future__ import annotations

import csv
import io
import json
import math
import os
import queue
import sys
import threading
import time
from typing import Any, Dict, List, Optional, Tuple

SURFACE = "dashboard"
MAX_ROWS = 20000
MAX_FILE_BYTES = 5 * 1024 * 1024
MAX_FILES = 21
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
SKIP_JSON = {
    "package-lock.json",
    "composer.lock",
    "tsconfig.tsbuildinfo",
    "tsconfig.json",
    "jsconfig.json",
    "app.json",
    "eas.json",
    "tauri.conf.json",
    "manifest.json",
    "package.json",
    "turbo.json",
    "lerna.json",
}
CONFIG_JSON_SUFFIXES = (".config.json", ".settings.json")
DATA_DIRS = {"data", "dataset", "datasets", "fixture", "fixtures", "sample", "samples", "report", "reports", "export", "exports"}
DATA_TOKENS = ("data", "sample", "report", "metric", "stat", "export", "order", "fixture", "dataset")
AGGREGATIONS = ("Average", "Sum", "Count", "Max", "Min")
CHART_TYPES = ("Bar", "Line", "Area")


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


def text(value: Any, size: str = "body", color: str = "ink") -> Dict[str, Any]:
    return {
        "kind": "text",
        "text": str(value).replace("\x00", "")[:16000],
        "size": size,
        "color": color,
    }


def card(title: str, children: List[Dict[str, Any]], gap: int = 10) -> Dict[str, Any]:
    return {
        "kind": "card",
        "gap": gap,
        "children": [text(title, "subtitle", "ink"), *children],
    }


def stat(
    label: str,
    value: str,
    detail: Optional[str] = None,
    color: str = "ink",
    trend: Optional[List[float]] = None,
) -> Dict[str, Any]:
    node: Dict[str, Any] = {
        "kind": "stat",
        "label": label,
        "value": value,
        "color": color,
    }
    if detail:
        node["detail"] = detail
    if trend:
        node["trend"] = trend
    return node


def table(columns: List[str], rows: List[List[Any]]) -> Dict[str, Any]:
    clean_rows = []
    for row in rows[:250]:
        clean_rows.append([str(cell).replace("\x00", "")[:2048] for cell in row[:24]])
    return {
        "kind": "table",
        "columns": [str(column).replace("\x00", "")[:256] for column in columns[:24]],
        "rows": clean_rows,
    }


def button(label: str, handler: str, disabled: bool = False) -> Dict[str, Any]:
    return {"kind": "button", "label": label, "handler": handler, "disabled": disabled}


def toggle(label: str, value: bool, handler: str) -> Dict[str, Any]:
    return {"kind": "toggle", "label": label, "value": value, "handler": handler}


def select(label: str, options: List[str], selected: int, handler: str) -> Dict[str, Any]:
    cleaned = [str(option).replace("\x00", "")[:240] for option in options[:24]]
    if not cleaned:
        cleaned = ["—"]
    return {
        "kind": "select",
        "label": label,
        "options": cleaned,
        "selected": max(0, min(selected, len(cleaned) - 1)),
        "handler": handler,
    }


def input_node(label: str, value: str, placeholder: str, handler: str) -> Dict[str, Any]:
    return {
        "kind": "input",
        "label": label,
        "value": value,
        "placeholder": placeholder,
        "handler": handler,
    }


def option_window(options: List[str], selected: int, limit: int = 24) -> Tuple[List[str], int]:
    """Keep a select readable while preserving the current choice."""
    if len(options) <= limit:
        return options, 0
    start = max(0, min(selected - limit // 2, len(options) - limit))
    window = options[start : start + limit]
    if selected < start or selected >= start + limit:
        window[-1] = options[selected]
        return window, start + len(window) - 1
    return window, start


def to_number(value: Any) -> Optional[float]:
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, (int, float)):
        number = float(value)
    else:
        raw = str(value).strip().replace(",", "")
        if not raw:
            return None
        percent = raw.endswith("%")
        if percent:
            raw = raw[:-1]
        raw = raw.replace("$", "").replace("€", "").replace("£", "")
        try:
            number = float(raw)
        except ValueError:
            return None
        if percent:
            number /= 100.0
    return number if math.isfinite(number) else None


def unique_label(value: Any) -> str:
    if value is None:
        return "(blank)"
    raw = str(value).replace("\x00", "").strip()
    return raw[:80] if raw else "(blank)"


class Dataset:
    def __init__(
        self,
        key: str,
        name: str,
        columns: List[str],
        rows: List[List[Any]],
        path: Optional[str] = None,
        size: int = 0,
        mtime: float = 0.0,
        sample: bool = False,
    ) -> None:
        self.key = key
        self.name = name
        self.columns = columns
        self.rows = rows
        self.path = path
        self.size = size
        self.mtime = mtime
        self.sample = sample
        self.numeric: List[int] = []
        self._detect_numeric()

    def _detect_numeric(self) -> None:
        self.numeric = []
        if not self.rows:
            return
        sample_rows = self.rows[: min(len(self.rows), 200)]
        for index in range(len(self.columns)):
            seen = 0
            numeric = 0
            for row in sample_rows:
                if index >= len(row):
                    continue
                value = row[index]
                if value is None or str(value).strip() == "":
                    continue
                seen += 1
                if to_number(value) is not None:
                    numeric += 1
            if seen and numeric / seen >= 0.8:
                self.numeric.append(index)

    def value(self, row: List[Any], index: int) -> Any:
        return row[index] if index < len(row) else None

    def missing(self) -> int:
        return sum(
            1
            for row in self.rows
            for index in range(len(self.columns))
            if self.value(row, index) is None or str(self.value(row, index)).strip() == ""
        )

    def column_values(self, index: int) -> List[Any]:
        return [self.value(row, index) for row in self.rows]


def make_release_sample() -> Dataset:
    rows: List[List[Any]] = []
    today = time.time()
    for offset in range(60):
        day = time.strftime("%Y-%m-%d", time.localtime(today - (59 - offset) * 86400))
        commits = round(18 + 9 * math.sin(offset / 5.0) + (offset % 7) * 1.4)
        issues = max(0, round(7 + 4 * math.cos(offset / 4.0) + (offset % 5)))
        coverage = round(71 + 6 * math.sin(offset / 9.0) + offset * 0.08, 1)
        failures = max(0, round(3 + 2 * math.cos(offset / 3.5)))
        rows.append([day, commits, issues, coverage, failures])
    return Dataset(
        "sample:release",
        "Sample · Release activity (60 days)",
        ["date", "commits", "issues_closed", "coverage_pct", "failed_builds"],
        rows,
        sample=True,
    )


def make_store_sample() -> Dataset:
    regions = ["North", "South", "West", "East", "Online"]
    channels = ["Web", "Mobile", "Partner"]
    rows: List[List[Any]] = []
    for index in range(96):
        region = regions[index % len(regions)]
        channel = channels[(index // 3) % len(channels)]
        orders = 40 + (index * 37) % 210
        revenue = round(orders * (28 + (index * 13) % 45) / 10.0, 2)
        refunds = round(revenue * ((index % 9) / 100.0), 2)
        days = 2 + (index * 7) % 14
        rows.append([region, channel, orders, revenue, refunds, days])
    return Dataset(
        "sample:store",
        "Sample · Store orders (96 rows)",
        ["region", "channel", "orders", "revenue", "refunds", "days_to_ship"],
        rows,
        sample=True,
    )


def make_fleet_sample() -> Dataset:
    rows: List[List[Any]] = []
    for index in range(36):
        host = f"worker-{index + 1:02d}"
        cpu = round(18 + (index * 17) % 76 + math.sin(index / 3.0) * 5, 1)
        memory = round(32 + (index * 11) % 61, 1)
        latency = round(4 + (index * 13) % 90 + math.cos(index / 4.0) * 6, 1)
        errors = round((index * 7) % 12 / 10.0, 2)
        rows.append([host, cpu, memory, latency, errors])
    return Dataset(
        "sample:fleet",
        "Sample · Fleet health (36 hosts)",
        ["host", "cpu_pct", "memory_pct", "latency_ms", "errors_per_min"],
        rows,
        sample=True,
    )


def samples() -> List[Dataset]:
    return [make_release_sample(), make_store_sample(), make_fleet_sample()]


def likely_data_json(path: str) -> bool:
    lower = path.lower().replace("\\", "/")
    parts = lower.split("/")
    if any(part in DATA_DIRS for part in parts[:-1]):
        return True
    stem = os.path.splitext(parts[-1])[0]
    return any(token in stem for token in DATA_TOKENS)


def discover_files(root: Optional[str]) -> List[str]:
    if not root or not os.path.isdir(root):
        return []
    found: List[Tuple[float, str]] = []
    for base, dirs, files in os.walk(root):
        relative = os.path.relpath(base, root)
        depth = 0 if relative == "." else relative.count(os.sep) + 1
        if depth > 4:
            dirs[:] = []
            continue
        dirs[:] = sorted(
            name
            for name in dirs
            if name not in SKIP_DIRS and not name.startswith(".")
        )
        for name in files:
            extension = os.path.splitext(name)[1].lower()
            if extension not in (".csv", ".tsv", ".json"):
                continue
            if name in SKIP_JSON or name.endswith(".min.json") or name == "plugin.json":
                continue
            if extension == ".json" and name.endswith(CONFIG_JSON_SUFFIXES):
                continue
            relative = os.path.relpath(os.path.join(base, name), root)
            if extension == ".json" and not likely_data_json(relative):
                continue
            path = os.path.join(base, name)
            try:
                info = os.stat(path)
            except OSError:
                continue
            if info.st_size == 0 or info.st_size > MAX_FILE_BYTES:
                continue
            found.append((info.st_mtime, path))
    found.sort(key=lambda item: (-item[0], item[1].lower()))
    return [path for _, path in found[:MAX_FILES]]


def clean_columns(columns: List[Any]) -> List[str]:
    cleaned: List[str] = []
    seen: Dict[str, int] = {}
    for index, column in enumerate(columns):
        name = str(column).strip() if column is not None else ""
        if not name:
            name = f"Column {index + 1}"
        name = name[:80]
        if name in seen:
            seen[name] += 1
            name = f"{name} ({seen[name]})"
        else:
            seen[name] = 0
        cleaned.append(name)
    return cleaned


def normalize_cell(value: Any) -> Any:
    if isinstance(value, (dict, list)):
        return json.dumps(value, separators=(",", ":"), ensure_ascii=False)[:400]
    return value


def load_csv(path: str, root: Optional[str]) -> Dataset:
    with open(path, "rb") as handle:
        raw = handle.read(MAX_FILE_BYTES + 1)
    if len(raw) > MAX_FILE_BYTES:
        raise ValueError("file is larger than 5 MB")
    text_data = raw.decode("utf-8-sig", errors="replace")
    sample = text_data[:8192]
    try:
        dialect = csv.Sniffer().sniff(sample, delimiters=",;\t|")
    except csv.Error:
        dialect = csv.excel_tab if path.lower().endswith(".tsv") else csv.excel
    reader = csv.reader(io.StringIO(text_data), dialect)
    rows = [row for row in reader if any(str(cell).strip() for cell in row)]
    if not rows:
        raise ValueError("file has no rows")
    columns = clean_columns(rows[0])
    body = rows[1 : MAX_ROWS + 1]
    width = len(columns)
    body = [row[:width] + [""] * max(0, width - len(row)) for row in body]
    name = os.path.basename(path)
    if root:
        name = os.path.relpath(path, root)
    info = os.stat(path)
    return Dataset(
        f"file:{path}",
        name,
        columns,
        body,
        path=path,
        size=info.st_size,
        mtime=info.st_mtime,
    )


def load_json(path: str, root: Optional[str]) -> Dataset:
    with open(path, "rb") as handle:
        raw = handle.read(MAX_FILE_BYTES + 1)
    if len(raw) > MAX_FILE_BYTES:
        raise ValueError("file is larger than 5 MB")
    value = json.loads(raw.decode("utf-8-sig", errors="replace"))
    if isinstance(value, dict):
        original = value
        candidates = [item for item in original.values() if isinstance(item, list)]
        value = next((item for item in candidates if item and isinstance(item[0], dict)), None)
        if value is None:
            value = next((item for item in candidates if item), None)
        if value is None:
            value = [{"key": key, "value": item} for key, item in original.items()]
    if not isinstance(value, list) or not value:
        raise ValueError("JSON must contain an array of objects or rows")
    if all(isinstance(item, dict) for item in value):
        columns: List[str] = []
        for item in value[:MAX_ROWS]:
            for key in item:
                if key not in columns:
                    columns.append(key)
            if len(columns) >= 60:
                break
        columns = clean_columns(columns[:40])
        rows = [
            [normalize_cell(item.get(column)) for column in columns]
            for item in value[:MAX_ROWS]
            if isinstance(item, dict)
        ]
    elif all(isinstance(item, list) for item in value):
        width = max(len(item) for item in value[:MAX_ROWS])
        columns = clean_columns([f"Column {index + 1}" for index in range(width)])
        rows = [
            [normalize_cell(item[index]) if index < len(item) else "" for index in range(width)]
            for item in value[:MAX_ROWS]
        ]
    else:
        columns = ["value"]
        rows = [[normalize_cell(item)] for item in value[:MAX_ROWS]]
    if not rows:
        raise ValueError("JSON has no usable rows")
    info = os.stat(path)
    name = os.path.basename(path)
    if root:
        name = os.path.relpath(path, root)
    return Dataset(
        f"file:{path}",
        name,
        columns,
        rows,
        path=path,
        size=info.st_size,
        mtime=info.st_mtime,
    )


def load_dataset(path: str, root: Optional[str]) -> Dataset:
    if os.path.splitext(path)[1].lower() in (".csv", ".tsv"):
        return load_csv(path, root)
    return load_json(path, root)


def fmt_size(size: int) -> str:
    if size >= 1024 * 1024:
        return f"{size / (1024 * 1024):.1f} MB"
    if size >= 1024:
        return f"{size / 1024:.1f} KB"
    return f"{size} B"


def fmt_number(value: float) -> str:
    if abs(value) >= 1_000_000:
        return f"{value / 1_000_000:.1f}m"
    if abs(value) >= 10_000:
        return f"{value / 1_000:.1f}k"
    if abs(value - round(value)) < 0.00001:
        return f"{value:,.0f}"
    if abs(value) >= 100:
        return f"{value:,.1f}"
    return f"{value:,.2f}"


def choose_defaults(dataset: Dataset) -> Tuple[int, int]:
    x = 0
    for index, column in enumerate(dataset.columns):
        values = dataset.column_values(index)
        seen = len({unique_label(value) for value in values})
        if index not in dataset.numeric and 1 < seen <= 30:
            x = index
            break
    metric = dataset.numeric[0] if dataset.numeric else 0
    return x, metric


def grouped_values(
    dataset: Dataset,
    x_index: int,
    metric_index: int,
    aggregation: int,
    top_n: int,
    limit: int = 3,
) -> Tuple[List[str], List[List[float]], Dict[str, float]]:
    """Group rows by the x column and aggregate one or more numeric measures."""
    x_values = dataset.column_values(x_index)
    numeric_x = all(to_number(value) is not None for value in x_values if value not in (None, ""))
    x_numbers = [to_number(value) for value in x_values]
    unique_count = len({unique_label(value) for value in x_values})
    use_buckets = numeric_x and unique_count > top_n and any(v is not None for v in x_numbers)

    grouped: Dict[str, List[int]] = {}
    bucket_order: List[str] = []
    if use_buckets:
        numbers = sorted(value for value in x_numbers if value is not None)
        low, high = numbers[0], numbers[-1]
        if high <= low:
            use_buckets = False
        else:
            width = (high - low) / top_n
            for row_index, value in enumerate(x_numbers):
                if value is None:
                    label = "(blank)"
                else:
                    bucket = min(top_n - 1, int((value - low) / width))
                    label = f"{fmt_number(low + bucket * width)}–{fmt_number(low + (bucket + 1) * width)}"
                if label not in grouped:
                    grouped[label] = []
                    bucket_order.append(label)
                grouped[label].append(row_index)
    if not use_buckets:
        for row_index, value in enumerate(x_values):
            label = unique_label(value)
            if label not in grouped:
                grouped[label] = []
                bucket_order.append(label)
            grouped[label].append(row_index)

    metrics = [metric_index] if metric_index in dataset.numeric else []
    if not metrics:
        metrics = dataset.numeric[:limit]
    if not metrics:
        metrics = [metric_index]
    metrics = metrics[:limit]

    if aggregation == 2 or not metrics:
        measures = [("Rows", [float(len(grouped[label])) for label in bucket_order])]
    else:
        measures = []
        for index in metrics:
            values: List[float] = []
            for label in bucket_order:
                numbers = []
                for row_index in grouped[label]:
                    if row_index < 0 or row_index >= len(dataset.rows):
                        continue
                    number = to_number(dataset.value(dataset.rows[row_index], index))
                    if number is not None:
                        numbers.append(number)
                if not numbers:
                    values.append(0.0)
                elif aggregation == 1:
                    values.append(sum(numbers))
                elif aggregation == 3:
                    values.append(max(numbers))
                elif aggregation == 4:
                    values.append(min(numbers))
                else:
                    values.append(sum(numbers) / len(numbers))
            column = dataset.columns[index] if index < len(dataset.columns) else "Value"
            measures.append((column, values))
    if not measures or not bucket_order:
        return [], [], {}

    # Bar charts read best largest-first; time-like charts keep their x order.
    scores = measures[0][1]
    order = list(range(len(bucket_order)))
    x_is_time = all(
        len(unique_label(value)) >= 7
        and (unique_label(value)[:4].isdigit() or "-" in unique_label(value))
        for value in x_values[:20]
        if value not in (None, "")
    )
    if x_is_time:
        order.sort(key=lambda index: bucket_order[index])
    elif numeric_x:
        order.sort(key=lambda index: to_number(bucket_order[index]) or 0.0)
    elif not numeric_x:
        order.sort(key=lambda index: -scores[index])
    labels = [bucket_order[index] for index in order][:top_n]
    metric_values = {
        label: score
        for label, score in zip(bucket_order, scores)
        if label in labels
    }
    series = [
        [values[index] for index in order][:top_n]
        for _, values in measures
    ]
    return labels, series, metric_values


def histogram_canvas(dataset: Dataset, metric_index: int) -> Optional[Dict[str, Any]]:
    values: List[float] = []
    for value in dataset.column_values(metric_index):
        number = to_number(value)
        if number is not None:
            values.append(number)
    if len(values) < 2:
        return None
    low, high = min(values), max(values)
    if high <= low:
        return None
    bins = 12
    counts = [0] * bins
    for value in values:
        index = min(bins - 1, int((value - low) / (high - low) * bins))
        counts[index] += 1
    peak = max(counts) or 1
    bar_width = (700 - (bins - 1) * 4) / bins
    colors = ["accent", "series2", "series4", "series5", "series6", "positive"]
    shapes: List[Dict[str, Any]] = []
    for index, count in enumerate(counts):
        height = 92.0 * count / peak
        x = 10 + index * (bar_width + 4)
        shapes.append(
            {
                "kind": "rect",
                "x": round(x, 2),
                "y": round(122 - height, 2),
                "width": round(bar_width, 2),
                "height": round(height, 2),
                "color": colors[index % len(colors)],
            }
        )
    for index in range(0, bins, 3):
        edge = low + (high - low) * index / (bins - 1)
        shapes.append(
            {
                "kind": "label",
                "x": round(10 + index * (bar_width + 4), 2),
                "y": 138,
                "text": fmt_number(edge),
                "color": "muted",
            }
        )
    shapes.append(
        {
            "kind": "label",
            "x": 10,
            "y": 10,
            "text": f"{fmt_number(low)} – {fmt_number(high)}",
            "color": "faint",
        }
    )
    return {"kind": "canvas", "width": 720, "height": 150, "shapes": shapes}


def correlation(dataset: Dataset, columns: List[int]) -> List[List[float]]:
    pairs: List[List[float]] = []
    for row in dataset.rows:
        values = [to_number(dataset.value(row, index)) for index in columns]
        if all(value is not None for value in values):
            pairs.append([float(value) for value in values if value is not None])
    if len(pairs) < 2:
        return []
    vectors = [[row[index] for row in pairs] for index in range(len(columns))]
    size = len(pairs)
    matrix: List[List[float]] = []
    for left in vectors:
        row = []
        for right in vectors:
            mean_left = sum(left) / size
            mean_right = sum(right) / size
            numerator = sum(
                (a - mean_left) * (b - mean_right) for a, b in zip(left, right)
            )
            denom_left = math.sqrt(sum((a - mean_left) ** 2 for a in left))
            denom_right = math.sqrt(sum((b - mean_right) ** 2 for b in right))
            denominator = denom_left * denom_right
            row.append(round(numerator / denominator, 3) if denominator else 0.0)
        matrix.append(row)
    return matrix


class Dashboard:
    def __init__(self) -> None:
        self.root: Optional[str] = None
        self.files: List[str] = []
        self.all_datasets: List[Dataset] = samples() + []
        self.current: Optional[Dataset] = None
        self.current_key = "sample:release"
        self.tab = 0
        self.x_index = 0
        self.metric_index = 0
        self.x_base = 0
        self.metric_base = 0
        self.aggregation = 0
        self.chart_type = 1
        self.top_n = 10
        self.compare = False
        self.live = False
        self.path_input = ""
        self.status = ""
        self.last_refresh = "—"
        self.loaded_at = time.time()
        self.next_id = 10
        self.persist_on_change = False
        self.current = self.find_dataset(self.current_key)
        self.apply_defaults()
        self.last_refresh = time.strftime("%H:%M:%S")

    def find_dataset(self, key: str) -> Optional[Dataset]:
        for dataset in self.all_datasets:
            if dataset.key == key:
                return dataset
        return None

    def apply_defaults(self) -> None:
        if not self.current:
            return
        self.x_index, self.metric_index = choose_defaults(self.current)
        if not self.current.numeric:
            self.metric_index = 0
            self.aggregation = 2
        else:
            self.aggregation = 0
        self.chart_type = 1

    def refresh_files(self) -> None:
        self.files = discover_files(self.root)
        self.rebuild_datasets()
        if self.current and self.current.path and self.current.path not in self.files:
            # Keep the active file visible even if discovery skipped it.
            self.files.insert(0, self.current.path)
            self.rebuild_datasets()

    def rebuild_datasets(self) -> None:
        base = samples()
        seen = {dataset.key for dataset in base}
        for path in self.files:
            key = f"file:{path}"
            if key in seen:
                continue
            name = os.path.relpath(path, self.root) if self.root else os.path.basename(path)
            try:
                size = os.stat(path).st_size
            except OSError:
                size = 0
            base.append(
                Dataset(
                    key,
                    f"File · {name}",
                    [],
                    [],
                    path=path,
                    size=size,
                    mtime=0.0,
                )
            )
            seen.add(key)
        current = self.current
        if current and current.path and current.key not in seen:
            base.append(current)
        self.all_datasets = base[:24]
        if self.current_key not in {item.key for item in self.all_datasets}:
            if current:
                self.all_datasets.append(current)
                self.all_datasets = self.all_datasets[-24:]

    def dataset_index(self) -> int:
        for index, dataset in enumerate(self.all_datasets):
            if dataset.key == self.current_key:
                return index
        return 0

    def select_dataset(self, key: str) -> None:
        if key.startswith("sample:"):
            dataset = self.find_dataset(key)
            if dataset:
                self.current_key = key
                self.current = dataset
                self.apply_defaults()
                self.status = ""
                self.loaded_at = time.time()
                self.last_refresh = time.strftime("%H:%M:%S")
            return
        path = key.removeprefix("file:")
        if not os.path.isfile(path):
            self.status = f"File not found: {path}"
            return
        try:
            dataset = load_dataset(path, self.root)
            self.current_key = key
            self.current = dataset
            self.apply_defaults()
            self.status = f"Loaded {dataset.name}"
            self.loaded_at = time.time()
            self.last_refresh = time.strftime("%H:%M:%S")
            if dataset.key not in {item.key for item in self.all_datasets}:
                self.rebuild_datasets()
        except Exception as error:  # noqa: BLE001 - surface file errors in the UI
            self.status = f"Cannot load {os.path.basename(path)}: {error}"

    def reload_current(self) -> bool:
        if not self.current or not self.current.path:
            return False
        try:
            info = os.stat(self.current.path)
        except OSError as error:
            self.status = f"Cannot stat active file: {error}"
            return False
        if abs(info.st_mtime - self.current.mtime) < 0.01:
            return False
        try:
            old_x = (
                self.current.columns[self.x_index]
                if self.x_index < len(self.current.columns)
                else None
            )
            old_metric = (
                self.current.columns[self.metric_index]
                if self.metric_index < len(self.current.columns)
                else None
            )
            dataset = load_dataset(self.current.path, self.root)
            self.current = dataset
            self.current_key = dataset.key
            if old_x in dataset.columns:
                self.x_index = dataset.columns.index(old_x)
            if old_metric in dataset.columns:
                self.metric_index = dataset.columns.index(old_metric)
            self.status = f"Reloaded {dataset.name} at {time.strftime('%H:%M:%S')}"
            self.last_refresh = time.strftime("%H:%M:%S")
            self.loaded_at = time.time()
            return True
        except Exception as error:  # noqa: BLE001
            self.status = f"Reload failed: {error}"
            return False

    def persist(self) -> None:
        if not self.persist_on_change:
            return
        request_id = self.next_id
        self.next_id += 1
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "storage.set",
                "params": {
                    "key": "dashboard",
                    "value": {
                        "dataset": self.current_key,
                        "tab": self.tab,
                        "chart": self.chart_type,
                        "aggregation": self.aggregation,
                        "top_n": self.top_n,
                        "compare": self.compare,
                        "live": self.live,
                        "x": self.x_index,
                        "metric": self.metric_index,
                    },
                },
            }
        )

    def restore(self, value: Any) -> None:
        if not isinstance(value, dict):
            return
        key = value.get("dataset")
        if isinstance(key, str) and (self.find_dataset(key) or key.startswith("file:")):
            self.select_dataset(key)
        self.tab = clamp_int(value.get("tab"), 0, 2, self.tab)
        self.chart_type = clamp_int(value.get("chart"), 0, 2, self.chart_type)
        self.aggregation = clamp_int(value.get("aggregation"), 0, 4, self.aggregation)
        self.top_n = clamp_int(value.get("top_n"), 3, 20, self.top_n)
        self.compare = bool(value.get("compare", self.compare))
        self.live = bool(value.get("live", self.live))
        if self.current:
            self.x_index = clamp_int(value.get("x"), 0, len(self.current.columns) - 1, self.x_index)
            self.metric_index = clamp_int(
                value.get("metric"), 0, len(self.current.columns) - 1, self.metric_index
            )
        self.persist_on_change = True

    def handle_event(self, handler: str, value: Any) -> None:
        if handler == "tab":
            self.tab = clamp_int(value, 0, 2, self.tab)
        elif handler == "dataset":
            index = clamp_int(value, 0, len(self.all_datasets) - 1, 0)
            self.select_dataset(self.all_datasets[index].key)
        elif handler == "dataset_base":
            pass
        elif handler == "refresh_files":
            if self.current and self.current.path:
                self.reload_current()
            self.refresh_files()
            self.status = f"Found {len(self.files)} CSV/JSON file(s)"
        elif handler == "live":
            self.live = bool(value)
            self.status = "Live reload is on" if self.live else "Live reload is off"
        elif handler == "x":
            self.x_index = clamp_int(
                self.x_base + int(value or 0),
                0,
                len(self.current.columns) - 1,
                self.x_index,
            )
        elif handler == "metric":
            if self.current and self.current.numeric:
                position = clamp_int(
                    self.metric_base + int(value or 0),
                    0,
                    len(self.current.numeric) - 1,
                    len(self.current.numeric) - 1,
                )
                self.metric_index = self.current.numeric[position]
            else:
                self.metric_index = 0
        elif handler == "aggregation":
            self.aggregation = clamp_int(value, 0, 4, self.aggregation)
        elif handler == "chart":
            self.chart_type = clamp_int(value, 0, 2, self.chart_type)
        elif handler == "top_n":
            self.top_n = clamp_int(value, 3, 20, self.top_n)
        elif handler == "compare":
            self.compare = bool(value)
        elif handler == "path":
            self.path_input = str(value or "")[:512]
        elif handler == "open_path":
            self.open_path()
        elif handler == "reset":
            self.apply_defaults()
            self.status = "View reset"
        elif handler == "refresh":
            self.reload_current()
            self.status = self.status or "Already up to date"
        self.persist()

    def open_path(self) -> None:
        if not self.root or not self.path_input.strip():
            self.status = "Enter a file path inside the project"
            return
        candidate = self.path_input.strip()
        if not os.path.isabs(candidate):
            candidate = os.path.join(self.root, candidate)
        candidate = os.path.realpath(candidate)
        root = os.path.realpath(self.root)
        try:
            inside = os.path.commonpath([candidate, root]) == root
        except ValueError:
            inside = False
        if not inside:
            self.status = "Only files inside the active project can be opened"
            return
        if not os.path.isfile(candidate):
            self.status = f"File not found: {self.path_input}"
            return
        self.files = [candidate] + [path for path in self.files if path != candidate]
        self.rebuild_datasets()
        self.select_dataset(f"file:{candidate}")
        self.path_input = ""

    def selected_metric(self) -> Optional[int]:
        if not self.current:
            return None
        if self.metric_index in self.current.numeric:
            return self.metric_index
        return self.current.numeric[0] if self.current.numeric else None

    def selected_x(self) -> int:
        if not self.current:
            return 0
        return max(0, min(self.x_index, len(self.current.columns) - 1))

    def chart_data(self) -> Tuple[List[str], List[Dict[str, Any]], Dict[str, float]]:
        if not self.current or not self.current.columns:
            return [], [], {}
        x_index = self.selected_x()
        metric = self.selected_metric()
        if metric is None:
            labels, values, metrics = grouped_values(
                self.current, x_index, 0, 2, self.top_n
            )
            series = [
                {
                    "label": "Rows",
                    "values": [round(value, 3) for value in values[0]],
                    "color": "accent",
                }
            ]
            return labels, series, metrics
        limit = min(3, len(self.current.numeric)) if self.compare else 1
        labels, values, metrics = grouped_values(
            self.current, x_index, metric, self.aggregation, self.top_n, limit
        )
        colors = ["accent", "series2", "series6"]
        names = ["Count"]
        if self.aggregation != 2 and self.compare:
            names = [
                self.current.columns[index]
                for index in self.current.numeric[:limit]
            ]
        elif self.aggregation != 2:
            names = [self.current.columns[metric]]
        series = []
        for index, row in enumerate(values[:3]):
            series.append(
                {
                    "label": names[index] if index < len(names) else f"Series {index + 1}",
                    "values": [round(value, 6) for value in row],
                    "color": colors[index],
                }
            )
        return labels, series, metrics

    def overview(self) -> Dict[str, Any]:
        dataset = self.current
        if not dataset or not dataset.columns or not dataset.rows:
            return card("No dataset", [text("No dataset loaded.", "body", "muted")])
        labels, series, _ = self.chart_data()
        missing = dataset.missing()
        cells = max(1, len(dataset.rows) * len(dataset.columns))
        completeness = max(0.0, min(100.0, 100.0 * (1 - missing / cells)))
        x_index = self.selected_x()
        x_values = dataset.column_values(x_index)
        counts: Dict[str, int] = {}
        for value in x_values:
            label = unique_label(value)
            counts[label] = counts.get(label, 0) + 1
        top_counts = sorted(counts.items(), key=lambda item: -item[1])[:5]
        slice_colors = ["accent", "series2", "series4", "series5", "series6"]
        donut_slices = [
            {
                "label": label[:32],
                "value": float(count),
                "color": slice_colors[index % len(slice_colors)],
            }
            for index, (label, count) in enumerate(top_counts)
        ]
        if not donut_slices:
            donut_slices = [{"label": "Rows", "value": 1.0, "color": "accent"}]
        numeric_columns = [dataset.columns[index] for index in dataset.numeric]
        heatmap_columns = dataset.numeric[:6]
        matrix = correlation(dataset, heatmap_columns)
        heatmap = None
        if matrix:
            heatmap = {
                "kind": "heatmap",
                "values": matrix,
                "rows": [dataset.columns[index][:24] for index in heatmap_columns],
                "columns": [dataset.columns[index][:24] for index in heatmap_columns],
                "color": "accent",
            }
        preview_columns = dataset.columns[:8]
        preview_rows = [
            [str(dataset.value(row, index))[:60] for index in range(len(preview_columns))]
            for row in dataset.rows[:8]
        ]
        stats = card(
            "Dataset",
            [
                table(
                    ["Metric", "Value", "Detail"],
                    [
                        ["Rows", f"{len(dataset.rows):,}", "capped at 20,000"],
                        ["Columns", str(len(dataset.columns)), f"{len(dataset.numeric)} numeric"],
                        [
                            "Missing cells",
                            f"{100 * missing / cells:.1f}%",
                            f"{missing:,} of {cells:,} cells",
                        ],
                        [
                            "Source",
                            fmt_size(dataset.size) if dataset.size else "sample",
                            "sample" if dataset.sample else "workspace file",
                        ],
                    ],
                )
            ],
        )
        charts = [
            card(
                "Selected view",
                [
                    text(
                        f"X: {dataset.columns[x_index]} · "
                        f"{AGGREGATIONS[self.aggregation]} "
                        f"{numeric_columns[0] if numeric_columns else 'rows'} · "
                        f"{CHART_TYPES[self.chart_type]}",
                        "small",
                        "muted",
                    ),
                    {
                        "kind": "chart",
                        "chart_type": CHART_TYPES[self.chart_type].lower(),
                        "labels": labels,
                        "series": series,
                        "height": 230,
                    },
                ],
            ),
            card(
                "Distribution",
                [
                    {
                        "kind": "donut",
                        "slices": donut_slices,
                        "label": dataset.columns[x_index],
                        "value": f"{len(counts)} groups",
                    },
                    {
                        "kind": "gauge",
                        "label": "Completeness",
                        "value": round(completeness, 1),
                        "min": 0,
                        "max": 100,
                        "unit": "%",
                        "color": "positive" if completeness >= 95 else "warning",
                    },
                ],
            ),
        ]
        lower = [
            card(
                "Preview",
                [
                    table(preview_columns, preview_rows),
                    text(
                        f"Showing {min(8, len(dataset.rows))} of {len(dataset.rows):,} rows.",
                        "caption",
                        "muted",
                    ),
                ],
            ),
            card(
                "Numeric correlation",
                [
                    heatmap
                    if heatmap
                    else text("Add at least two numeric columns.", "body", "muted")
                ],
            ),
        ]
        return {
            "kind": "column",
            "gap": 12,
            "children": [stats, *charts, *lower],
        }

    def explore(self) -> Dict[str, Any]:
        dataset = self.current
        if not dataset or not dataset.rows:
            return card("No dataset", [text("Load a CSV or JSON file first.", "body", "muted")])
        labels, series, _ = self.chart_data()
        x_index = self.selected_x()
        metric = self.selected_metric()
        aggregation = self.aggregation
        grouped_rows = []
        for index, label in enumerate(labels):
            row: List[Any] = [label]
            for measure in series:
                values = measure.get("values", [])
                row.append(fmt_number(values[index]) if index < len(values) else "0")
            grouped_rows.append(row)
        grouped_columns = ["Group"] + [measure.get("label", "Value") for measure in series]
        metric_name = (
            "Rows" if aggregation == 2 or metric is None else dataset.columns[metric]
        )
        x_options, self.x_base = option_window(
            [column[:80] for column in dataset.columns], x_index
        )
        metric_positions = dataset.numeric if dataset.numeric else []
        metric_labels = (
            [dataset.columns[index][:80] for index in metric_positions]
            if metric_positions
            else ["Rows"]
        )
        metric_selected = (
            metric_positions.index(metric)
            if metric is not None and metric in metric_positions
            else 0
        )
        metric_options, self.metric_base = option_window(metric_labels, metric_selected)
        metric_selected -= self.metric_base
        top_values = sorted(
            (value for value in (series[0].get("values", []) if series else []) if value > 0),
            reverse=True,
        )
        top_share = (
            (top_values[0] / sum(top_values) * 100) if top_values and sum(top_values) else 0
        )
        histogram = histogram_canvas(dataset, metric) if metric is not None else None
        controls = {
            "kind": "column",
            "gap": 8,
            "children": [
                select("X axis", x_options, x_index, "x"),
                select("Measure", metric_options, metric_selected, "metric"),
                select("Aggregation", list(AGGREGATIONS), aggregation, "aggregation"),
                select("Chart type", list(CHART_TYPES), self.chart_type, "chart"),
            ],
        }
        return {
            "kind": "column",
            "gap": 12,
            "children": [
                card("View", [controls]),
                card(
                    f"{CHART_TYPES[self.chart_type]} of {aggregation == 2 and 'rows' or metric_name} by {dataset.columns[x_index]}",
                    [
                        {
                            "kind": "chart",
                            "chart_type": CHART_TYPES[self.chart_type].lower(),
                            "labels": labels,
                            "series": series,
                            "height": 320,
                        },
                    ],
                ),
                *(
                    [
                        card(
                            f"Raw distribution · {metric_name}",
                            [histogram],
                        )
                    ]
                    if histogram
                    else []
                ),
                card(
                    "Grouped values",
                    [
                        table(grouped_columns, grouped_rows),
                        text(
                            f"Top group: {top_share:.1f}% of the plotted measure.",
                            "caption",
                            "muted",
                        ),
                    ],
                ),
                card(
                    "View controls",
                    [
                        {
                            "kind": "slider",
                            "label": f"Top groups ({self.top_n})",
                            "value": self.top_n,
                            "min": 3,
                            "max": 20,
                            "step": 1,
                            "handler": "top_n",
                        },
                        toggle("Compare up to 3 measures", self.compare, "compare"),
                        {
                            "kind": "progress",
                            "label": "Groups shown",
                            "value": len(labels),
                            "max": 20,
                            "detail": f"{len(labels)} of up to 20 groups",
                            "color": "series2",
                        },
                        {
                            "kind": "button",
                            "label": "Reset view",
                            "handler": "reset",
                            "disabled": False,
                        },
                    ],
                ),
            ],
        }

    def data_tab(self) -> Dict[str, Any]:
        dataset = self.current
        if not dataset or not dataset.rows:
            return card(
                "Open a file",
                [
                    text("CSV, TSV, or JSON inside the project.", "body", "muted"),
                    {
                        "kind": "row",
                        "gap": 8,
                        "children": [
                            input_node("File path", self.path_input, "data/orders.csv", "path"),
                            button("Open path", "open_path"),
                        ],
                    },
                ],
            )
        preview_columns = dataset.columns[:12]
        preview_rows = [
            [str(dataset.value(row, index))[:80] for index in range(len(preview_columns))]
            for row in dataset.rows[:50]
        ]
        profile_rows: List[List[Any]] = []
        for index, column in enumerate(dataset.columns):
            values = dataset.column_values(index)
            present = [value for value in values if value is not None and str(value).strip()]
            missing = len(values) - len(present)
            unique = len({unique_label(value) for value in values})
            if index in dataset.numeric:
                numbers = [to_number(value) for value in present]
                numbers = [value for value in numbers if value is not None]
                if numbers:
                    profile_rows.append(
                        [
                            column,
                            "number",
                            str(missing),
                            str(unique),
                            fmt_number(min(numbers)),
                            fmt_number(max(numbers)),
                            fmt_number(sum(numbers) / len(numbers)),
                        ]
                    )
                else:
                    profile_rows.append([column, "number", str(missing), str(unique), "—", "—", "—"])
            else:
                profile_rows.append([column, "text", str(missing), str(unique), "—", "—", "—"])
        inventory_rows = []
        for item in self.all_datasets[:20]:
            inventory_rows.append(
                [
                    item.name,
                    "sample" if item.sample else "file",
                    f"{len(item.rows):,}" if item.rows else "—",
                    str(len(item.columns)) if item.columns else "—",
                    fmt_size(item.size) if item.size else "—",
                ]
            )
        return {
            "kind": "column",
            "gap": 12,
            "children": [
                card(
                    "Open a file",
                    [
                        {
                            "kind": "row",
                            "gap": 8,
                            "children": [
                                input_node("File path", self.path_input, "data/orders.csv", "path"),
                                button("Open path", "open_path"),
                                button("Rescan workspace", "refresh_files"),
                            ],
                        },
                    ],
                ),
                card(
                    "Preview (first 50 rows)",
                    [
                        table(preview_columns, preview_rows),
                        text(
                            f"Showing {min(50, len(dataset.rows))} of {len(dataset.rows):,} rows.",
                            "caption",
                            "muted",
                        ),
                    ],
                ),
                card(
                    "Column profile",
                    [
                        table(
                            ["Column", "Type", "Missing", "Unique", "Min", "Max", "Mean"],
                            profile_rows,
                        )
                    ],
                ),
                card("Dataset inventory", [table(["Dataset", "Kind", "Rows", "Columns", "Size"], inventory_rows)]),
            ],
        }

    def header(self) -> List[Dict[str, Any]]:
        values = [dataset.name for dataset in self.all_datasets[:24]]
        selected = self.dataset_index()
        dataset_select = select("Dataset", values or ["Sample · Release activity"], selected, "dataset")
        dataset_select["selected"] = max(0, min(selected, len(dataset_select["options"]) - 1))
        return [
            dataset_select,
            {
                "kind": "row",
                "gap": 8,
                "children": [
                    button("Refresh file", "refresh", disabled=not (self.current and self.current.path)),
                    button("Rescan", "refresh_files"),
                    toggle("Live", self.live, "live"),
                ],
            },
        ]

    def tree(self) -> Dict[str, Any]:
        body: Dict[str, Any]
        if self.tab == 0:
            body = self.overview()
        elif self.tab == 1:
            body = self.explore()
        else:
            body = self.data_tab()
        content: List[Dict[str, Any]] = [*self.header()]
        if self.status:
            content.append(text(self.status, "small", "muted"))
        content.append(body)
        return {
            "kind": "page_tabs",
            "labels": ["Overview", "Explore", "Data"],
            "selected": self.tab,
            "handler": "tab",
            "nav_width": 210,
            "children": [{"kind": "column", "gap": 12, "children": content}],
        }

    def publish(self) -> None:
        notify("ui.set", {"surface": SURFACE, "tree": self.tree()})


def clamp_int(value: Any, low: int, high: int, fallback: int) -> int:
    try:
        number = int(value)
    except (TypeError, ValueError):
        return fallback
    return max(low, min(high, number))


def main() -> None:
    incoming: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    threading.Thread(target=read_stdin, args=(incoming,), daemon=True).start()
    state = Dashboard()
    last_live_check = 0.0

    while True:
        try:
            message = incoming.get(timeout=0.4)
        except queue.Empty:
            now = time.monotonic()
            if state.live and now - last_live_check > 1.0:
                last_live_check = now
                if state.reload_current():
                    state.publish()
            continue

        method = message.get("method")
        if method is None:
            if message.get("id") == 2:
                result = message.get("result") or {}
                root = result.get("root")
                state.root = str(root) if root else None
                state.refresh_files()
                state.publish()
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "storage.get",
                        "params": {"key": "dashboard"},
                    }
                )
            elif message.get("id") == 3:
                state.restore(message.get("result"))
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
                            "title": "Dashboard",
                            "icon": "dashboard",
                            "order": 1,
                        }
                    ],
                },
            )
            state.publish()
            send(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "workspace.info",
                    "params": {},
                }
            )
        elif method == "ui.open":
            state.root = None
            send(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "workspace.info",
                    "params": {},
                }
            )
            state.publish()
        elif method == "ui.event":
            params = message.get("params") or {}
            if params.get("surface") != SURFACE:
                continue
            state.handle_event(
                str(params.get("handler") or ""),
                params.get("value"),
            )
            state.publish()
        elif method == "shutdown":
            break


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


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        raise SystemExit(0)
