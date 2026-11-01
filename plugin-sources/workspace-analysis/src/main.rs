//! Workspace analysis and repository insights for any project.
//!
//! Walks the active workspace, extracts git commit and line-churn history,
//! detects languages, modules, manifests, and toolchains, and publishes a
//! bounded native UI report over stdio JSON-RPC 2.0. Never modifies the
//! workspace. stdout is protocol traffic only; diagnostics go to stderr.

use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SURFACE: &str = "workspace-analysis";
const MAX_FILES: usize = 50_000;
const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILE_READ: u64 = 512 * 1024;
const MAX_TODOS: usize = 200;
const MAX_WALK_DEPTH: usize = 10;
const CALENDAR_WEEKS: usize = 52;
const CHURN_WEEKS: usize = 16;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".next",
    ".turbo",
    "coverage",
    ".idea",
    ".cache",
    "Pods",
    "DerivedData",
];

const MODULE_CONTAINERS: &[&str] = &[
    "crates",
    "packages",
    "apps",
    "services",
    "plugins",
    "plugin-sources",
    "libs",
    "modules",
    "cmd",
    "pkg",
    "internal",
];

const LOCKFILE_NAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "go.sum",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
];

#[derive(Clone, Default)]
struct FileInfo {
    path: String,
    size: u64,
    lines: usize,
    source_lines: usize,
    test_lines: usize,
    language: String,
    source: bool,
    is_test: bool,
    todo: usize,
    large: bool,
    commits_90d: usize,
    churn_90d: usize,
}

#[derive(Clone, Default)]
struct LanguageStats {
    files: usize,
    lines: usize,
    bytes: u64,
    source: bool,
}

#[derive(Clone, Default)]
struct ModuleStats {
    name: String,
    files: usize,
    source_lines: usize,
    test_lines: usize,
    todos: usize,
    added_30d: usize,
    deleted_30d: usize,
    lang_lines: BTreeMap<String, usize>,
}

impl ModuleStats {
    fn primary_language(&self) -> String {
        self.lang_lines
            .iter()
            .max_by_key(|(_, lines)| *lines)
            .map(|(lang, _)| language_label(lang))
            .unwrap_or_else(|| language_label("Other"))
    }
}

#[derive(Clone)]
struct Manifest {
    path: String,
    ecosystem: String,
    name: String,
    frameworks: Vec<String>,
    deps: Option<usize>,
    resolved: Option<usize>,
}

#[derive(Clone)]
struct Todo {
    path: String,
    line: usize,
    tag: String,
    text: String,
}

#[derive(Clone)]
struct Tool {
    name: String,
    ecosystem: String,
    available: bool,
    version: String,
}

#[derive(Clone, Default)]
struct GitInfo {
    repo: bool,
    branch: String,
    dirty: usize,
    last_commit: String,
    error: String,
}

#[derive(Clone)]
struct GitHistory {
    calendar: Vec<Vec<f64>>,
    month_columns: Vec<String>,
    window_commits: usize,
    active_days: usize,
    window_end_label: Option<String>,
    churn_labels: Vec<String>,
    weekly_added: Vec<f64>,
    weekly_deleted: Vec<f64>,
    total_added_16w: usize,
    total_deleted_16w: usize,
    file_commits_90d: HashMap<String, usize>,
    file_churn_90d: HashMap<String, usize>,
    file_added_30d: HashMap<String, usize>,
    file_deleted_30d: HashMap<String, usize>,
}

impl Default for GitHistory {
    fn default() -> Self {
        Self {
            calendar: vec![vec![0.0; CALENDAR_WEEKS]; 7],
            month_columns: vec![String::new(); CALENDAR_WEEKS],
            window_commits: 0,
            active_days: 0,
            window_end_label: None,
            churn_labels: vec![String::new(); CHURN_WEEKS],
            weekly_added: vec![0.0; CHURN_WEEKS],
            weekly_deleted: vec![0.0; CHURN_WEEKS],
            total_added_16w: 0,
            total_deleted_16w: 0,
            file_commits_90d: HashMap::new(),
            file_churn_90d: HashMap::new(),
            file_added_30d: HashMap::new(),
            file_deleted_30d: HashMap::new(),
        }
    }
}

#[derive(Clone, Default)]
struct Analysis {
    files: Vec<FileInfo>,
    languages: BTreeMap<String, LanguageStats>,
    modules: Vec<ModuleStats>,
    manifests: Vec<Manifest>,
    todos: Vec<Todo>,
    todo_total: usize,
    todo_tags: BTreeMap<String, usize>,
    tools: Vec<Tool>,
    alerts: Vec<String>,
    git: GitInfo,
    history: GitHistory,
    lockfile_ok: bool,
    truncated: bool,
    scanned_ms: u128,
    scanned_at: String,
    total_lines: usize,
    source_lines: usize,
    test_lines: usize,
    total_bytes: u64,
}

struct State {
    root: Option<PathBuf>,
    analysis: Option<Analysis>,
    tab: usize,
    status: String,
}

impl State {
    fn new() -> Self {
        Self {
            root: None,
            analysis: None,
            tab: 0,
            status: "Waiting for active workspace...".into(),
        }
    }

    fn rescan(&mut self) {
        let Some(root) = self.root.clone() else {
            self.status = "No active workspace".into();
            return;
        };
        if !root.is_dir() {
            self.status = format!("Workspace is not a directory: {}", root.display());
            return;
        }
        let scan = analyze(&root);
        self.status = status_line(&scan);
        self.analysis = Some(scan);
    }

    fn set_workspace_root(&mut self, root: PathBuf) -> bool {
        let changed = self.root.as_ref() != Some(&root);
        self.root = Some(root);
        if changed || self.analysis.is_none() {
            self.rescan();
            true
        } else {
            false
        }
    }

    fn handle_event(&mut self, handler: &str, value: &Value) {
        match handler {
            "tab" => {
                self.tab = value.as_u64().unwrap_or_default().min(4) as usize;
            }
            "refresh" => self.rescan(),
            _ => {}
        }
    }
}

fn status_line(analysis: &Analysis) -> String {
    let git_part = if analysis.git.repo {
        let tree_state = if analysis.git.dirty == 0 {
            "clean".to_string()
        } else {
            format!("{} modified", analysis.git.dirty)
        };
        format!("{} · {tree_state}", analysis.git.branch)
    } else {
        "no git".to_string()
    };
    let truncated_suffix = if analysis.truncated {
        " · truncated"
    } else {
        ""
    };
    format!(
        "{git_part} · {} files · {} LOC · {} ms · {}{truncated_suffix}",
        fmt_int(analysis.files.len()),
        fmt_int(analysis.total_lines),
        analysis.scanned_ms,
        analysis.scanned_at
    )
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn language_for(name: &str) -> (&'static str, bool) {
    let lower = name.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") {
        return ("Docker", true);
    }
    if lower == "makefile" || lower == "gnumakefile" {
        return ("Make", true);
    }
    if LOCKFILE_NAMES
        .iter()
        .any(|lock| lower == lock.to_ascii_lowercase())
        || lower.ends_with(".lock")
    {
        return ("Lockfile", false);
    }
    let extension = lower.rsplit('.').next().unwrap_or("");
    match extension {
        "rs" => ("Rust", true),
        "py" | "pyi" => ("Python", true),
        "go" => ("Go", true),
        "js" | "jsx" | "mjs" | "cjs" => ("JavaScript", true),
        "ts" | "tsx" | "mts" | "cts" => ("TypeScript", true),
        "java" => ("Java", true),
        "kt" | "kts" => ("Kotlin", true),
        "swift" => ("Swift", true),
        "c" | "h" => ("C", true),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => ("C++", true),
        "cs" => ("C#", true),
        "rb" => ("Ruby", true),
        "php" => ("PHP", true),
        "lua" => ("Lua", true),
        "ex" | "exs" => ("Elixir", true),
        "zig" => ("Zig", true),
        "sh" | "bash" | "zsh" | "fish" => ("Shell", true),
        "sql" => ("SQL", true),
        "html" | "htm" => ("HTML", true),
        "css" | "scss" | "sass" | "less" => ("CSS", true),
        "md" | "mdx" | "rst" => ("Markdown", false),
        "json" | "jsonc" => ("JSON", false),
        "toml" => ("TOML", false),
        "yaml" | "yml" => ("YAML", false),
        "xml" => ("XML", false),
        "txt" => ("Text", false),
        _ => ("Other", false),
    }
}

fn language_kind(name: &str, source: bool) -> &'static str {
    if source {
        "source"
    } else {
        match name {
            "Markdown" | "Text" => "docs",
            "JSON" | "TOML" | "YAML" | "XML" => "config",
            "Lockfile" => "lock",
            _ => "other",
        }
    }
}

fn language_icon(name: &str) -> &'static str {
    match name {
        "Rust" => "🦀",
        "Python" => "🐍",
        "Go" => "🐹",
        "JavaScript" => "🟨",
        "TypeScript" => "🟦",
        "Java" => "☕",
        "Kotlin" => "🟪",
        "Swift" => "🐦",
        "C" => "🔧",
        "C++" => "🔩",
        "C#" => "🎯",
        "Ruby" => "💎",
        "PHP" => "🐘",
        "Lua" => "🌙",
        "Elixir" => "💧",
        "Zig" => "⚡",
        "Shell" => "🐚",
        "SQL" => "🗄",
        "HTML" => "🧱",
        "CSS" => "🎨",
        "Docker" => "🐳",
        "Make" => "🛠",
        "Markdown" => "📝",
        "JSON" => "🧾",
        "YAML" => "⚙",
        "TOML" => "⚙",
        "XML" => "📐",
        "Lockfile" => "🔒",
        "Text" => "📄",
        _ => "📦",
    }
}

fn language_label(name: &str) -> String {
    format!("{} {}", language_icon(name), name)
}

fn ecosystem_label(ecosystem: &str) -> String {
    let icon = match ecosystem {
        "Rust" => "🦀",
        "Node" => "🟩",
        "Go" => "🐹",
        "Python" => "🐍",
        "Ruby" => "💎",
        "PHP" => "🐘",
        "Java" => "☕",
        "Java/Kotlin" => "🟪",
        "C/C++" => "🔩",
        "Make" => "🛠",
        "Docker" => "🐳",
        _ => "📦",
    };
    format!("{icon} {ecosystem}")
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut segments = lower.split('/').peekable();
    while let Some(seg) = segments.next() {
        let is_last = segments.peek().is_none();
        if !is_last
            && matches!(
                seg,
                "test" | "tests" | "__tests__" | "spec" | "specs" | "benches"
            )
        {
            return true;
        }
        if is_last
            && (matches!(seg, "test.rs" | "tests.rs" | "test.py" | "tests.py")
                || seg.starts_with("test_")
                || seg.ends_with("_test.rs")
                || seg.ends_with("_tests.rs")
                || seg.ends_with("_test.go")
                || seg.ends_with("_test.py")
                || seg.ends_with(".test.js")
                || seg.ends_with(".test.ts")
                || seg.ends_with(".test.jsx")
                || seg.ends_with(".test.tsx")
                || seg.ends_with(".spec.js")
                || seg.ends_with(".spec.ts")
                || seg.ends_with(".spec.jsx")
                || seg.ends_with(".spec.tsx"))
        {
            return true;
        }
    }
    false
}

fn is_lockfile_or_ignored_churn(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    if LOCKFILE_NAMES.contains(&base) || base.ends_with(".lock") {
        return true;
    }
    path.split('/').any(|seg| SKIP_DIRS.contains(&seg))
}

fn module_for(path: &str, expand_src: bool) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return ".".into();
    }
    if parts.len() >= 3 && MODULE_CONTAINERS.contains(&parts[0]) {
        return format!("{}/{}", parts[0], parts[1]);
    }
    if expand_src && parts[0] == "src" && parts.len() >= 3 {
        return format!("src/{}", parts[1]);
    }
    parts[0].to_string()
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn comment_leader_offset(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('*') {
        return Some(line.len() - trimmed.len());
    }
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if b == b'\\' && (in_single || in_double) {
            idx += 2;
            continue;
        }
        if b == b'"' && !in_single {
            in_double = !in_double;
            idx += 1;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            idx += 1;
            continue;
        }
        if !in_single && !in_double {
            let rest = &bytes[idx..];
            if rest.starts_with(b"//")
                || rest.starts_with(b"/*")
                || rest.starts_with(b"<!--")
                || (b == b'#' && (idx == 0 || bytes[idx - 1].is_ascii_whitespace()))
            {
                return Some(idx);
            }
        }
        idx += 1;
    }
    None
}

fn is_inside_quotes(line: &str, target: usize) -> bool {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut idx = 0usize;
    while idx < target.min(bytes.len()) {
        let b = bytes[idx];
        if b == b'\\' && (in_single || in_double) {
            idx += 2;
            continue;
        }
        if b == b'"' && !in_single {
            in_double = !in_double;
        } else if b == b'\'' && !in_double {
            in_single = !in_single;
        }
        idx += 1;
    }
    in_single || in_double
}

fn find_marker_tag(line: &str) -> Option<&'static str> {
    if !line.contains("TODO")
        && !line.contains("FIXME")
        && !line.contains("HACK")
        && !line.contains("XXX")
    {
        return None;
    }
    const TAGS: [&str; 4] = ["TODO", "FIXME", "HACK", "XXX"];
    let bytes = line.as_bytes();
    let comment_at = comment_leader_offset(line);
    for tag in TAGS {
        let tbytes = tag.as_bytes();
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(tag) {
            let pos = search_from + rel;
            let end = pos + tbytes.len();
            search_from = end;

            let left_ok = pos == 0 || !is_word_byte(bytes[pos - 1]);
            let right_ok = end == bytes.len() || !is_word_byte(bytes[end]);
            if !left_ok || !right_ok {
                continue;
            }
            let quoted = pos > 0
                && end < bytes.len()
                && ((bytes[pos - 1] == b'"' && bytes[end] == b'"')
                    || (bytes[pos - 1] == b'\'' && bytes[end] == b'\'')
                    || (bytes[pos - 1] == b'`' && bytes[end] == b'`'));
            if quoted {
                continue;
            }
            let in_comment = comment_at.is_some_and(|start| pos >= start);
            let after = line[end..].trim_start();
            let has_delimiter = after.starts_with(':') || after.starts_with('(');
            if in_comment || (!is_inside_quotes(line, pos) && has_delimiter) {
                return Some(tag);
            }
        }
    }
    None
}

fn scan_content(
    path: &str,
    text: &str,
    file: &mut FileInfo,
    todos: &mut Vec<Todo>,
    tag_counts: &mut BTreeMap<String, usize>,
) {
    file.lines = count_lines(text);
    let mut in_inline_rust_tests = false;
    let mut source_lines = 0usize;
    let mut test_lines = 0usize;

    for (index, line) in text.lines().enumerate() {
        if !file.is_test && file.language == "Rust" && line.trim() == "#[cfg(test)]" {
            in_inline_rust_tests = true;
        }
        if file.is_test || in_inline_rust_tests {
            test_lines += 1;
        } else if file.source {
            source_lines += 1;
        }

        if !file.source || file.todo >= 100 {
            continue;
        }
        if let Some(tag) = find_marker_tag(line) {
            file.todo += 1;
            *tag_counts.entry(tag.to_string()).or_default() += 1;
            if todos.len() < MAX_TODOS {
                let excerpt = line
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(140)
                    .collect::<String>();
                todos.push(Todo {
                    path: path.to_string(),
                    line: index + 1,
                    tag: tag.into(),
                    text: excerpt,
                });
            }
        }
    }

    file.source_lines = source_lines;
    file.test_lines = test_lines;
}

fn walk(root: &Path) -> (Vec<FileInfo>, bool) {
    let mut files = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut truncated = false;
    while let Some((directory, depth)) = queue.pop_front() {
        if depth > MAX_WALK_DEPTH {
            continue;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                queue.push_back((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if files.len() >= MAX_FILES {
                truncated = true;
                return (files, truncated);
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let (language, source) = language_for(&name);
            let is_test = source && is_test_path(&relative);
            files.push(FileInfo {
                path: relative,
                size: metadata.len(),
                language: language.into(),
                source,
                is_test,
                large: metadata.len() > 2 * 1024 * 1024,
                ..Default::default()
            });
        }
    }
    (files, truncated)
}

fn analyze(root: &Path) -> Analysis {
    let started = Instant::now();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let (mut files, truncated) = walk(root);
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let has_workspace_containers = files.iter().any(|file| {
        let mut parts = file.path.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(first), Some(_), Some(_)) => MODULE_CONTAINERS.contains(&first),
            _ => false,
        }
    });
    let expand_src = !has_workspace_containers;

    let mut languages: BTreeMap<String, LanguageStats> = BTreeMap::new();
    let mut manifests = Vec::new();
    let mut todos = Vec::new();
    let mut todo_tags_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut total_lines = 0usize;
    let mut source_lines = 0usize;
    let mut test_lines = 0usize;
    let mut read_budget = MAX_CONTENT_BYTES;

    for file in &mut files {
        total_bytes += file.size;
        let entry = languages.entry(file.language.clone()).or_default();
        entry.files += 1;
        entry.bytes += file.size;
        entry.source |= file.source;

        if manifest_kind(&file.path).is_some() && manifests.len() < 40 {
            manifests.push(read_manifest(root, &file.path));
        }

        let can_read = file.size <= MAX_FILE_READ
            && (file.source
                || manifest_kind(&file.path).is_some()
                || matches!(
                    file.language.as_str(),
                    "Markdown" | "JSON" | "TOML" | "YAML" | "XML" | "Text"
                ))
            && read_budget > 0;
        if !can_read {
            if file.size > MAX_FILE_READ && file.source {
                if let Ok(handle) = File::open(root.join(&file.path)) {
                    let mut lines = 0usize;
                    let mut reader = BufReader::new(handle);
                    let mut buffer = String::new();
                    while reader.read_line(&mut buffer).unwrap_or(0) > 0 {
                        lines += 1;
                        if lines > 2_000_000 {
                            break;
                        }
                        buffer.clear();
                    }
                    file.lines = lines;
                    if file.is_test {
                        file.test_lines = lines;
                    } else {
                        file.source_lines = lines;
                    }
                    entry.lines += file.lines;
                    total_lines += file.lines;
                    source_lines += file.source_lines;
                    test_lines += file.test_lines;
                }
            }
            continue;
        }
        let Ok(bytes) = read_limited(&root.join(&file.path), MAX_FILE_READ) else {
            continue;
        };
        read_budget = read_budget.saturating_sub(bytes.len() as u64);
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let relative = file.path.clone();
        scan_content(&relative, &text, file, &mut todos, &mut todo_tags_map);
        entry.lines += file.lines;
        total_lines += file.lines;
        source_lines += file.source_lines;
        test_lines += file.test_lines;
    }

    let git = git_info(root);
    let history = if git.repo {
        git_history(root, now_secs)
    } else {
        GitHistory::default()
    };

    let mut module_map: BTreeMap<String, ModuleStats> = BTreeMap::new();
    for file in &mut files {
        file.commits_90d = history
            .file_commits_90d
            .get(&file.path)
            .copied()
            .unwrap_or(0);
        file.churn_90d = history.file_churn_90d.get(&file.path).copied().unwrap_or(0);

        let mod_name = module_for(&file.path, expand_src);
        let module = module_map
            .entry(mod_name.clone())
            .or_insert_with(|| ModuleStats {
                name: mod_name,
                ..Default::default()
            });
        module.files += 1;
        module.source_lines += file.source_lines;
        module.test_lines += file.test_lines;
        module.todos += file.todo;
        if file.lines > 0 && file.language != "Lockfile" && file.language != "Other" {
            let weight = if file.source {
                file.lines * 4
            } else {
                file.lines
            };
            *module.lang_lines.entry(file.language.clone()).or_default() += weight;
        }
    }

    for (path, added) in &history.file_added_30d {
        let mod_name = module_for(path, expand_src);
        if let Some(module) = module_map.get_mut(&mod_name) {
            module.added_30d += *added;
        }
    }
    for (path, deleted) in &history.file_deleted_30d {
        let mod_name = module_for(path, expand_src);
        if let Some(module) = module_map.get_mut(&mod_name) {
            module.deleted_30d += *deleted;
        }
    }

    let mut modules: Vec<ModuleStats> = module_map.into_values().collect();
    modules.sort_by(|a, b| {
        let a_loc = a.source_lines + a.test_lines;
        let b_loc = b.source_lines + b.test_lines;
        b_loc
            .cmp(&a_loc)
            .then_with(|| (b.added_30d + b.deleted_30d).cmp(&(a.added_30d + a.deleted_30d)))
            .then_with(|| b.files.cmp(&a.files))
    });

    let tools = toolchains(&manifests);
    let has_lockfile = LOCKFILE_NAMES
        .iter()
        .any(|name| root.join(name).is_file())
        || files
            .iter()
            .any(|file| LOCKFILE_NAMES.iter().any(|name| file.path.ends_with(name)));
    let ecosystems: BTreeSet<String> = manifests.iter().map(|m| m.ecosystem.clone()).collect();
    let needs_lockfile = ecosystems
        .iter()
        .any(|eco| matches!(eco.as_str(), "Rust" | "Node" | "Go" | "Python"));
    let lockfile_ok = !needs_lockfile || has_lockfile;
    let todo_total: usize = files.iter().map(|file| file.todo).sum();

    let mut alerts = Vec::new();
    for tool in &tools {
        if !tool.available {
            alerts.push(format!("Missing toolchain: {} ({})", tool.name, tool.ecosystem));
        }
    }
    if !lockfile_ok {
        alerts.push("No lockfile detected for package manifests".into());
    }
    if truncated {
        alerts.push(format!("File walk capped at {} files", fmt_int(MAX_FILES)));
    }
    let large_files = files.iter().filter(|file| file.large).count();
    if large_files > 0 {
        alerts.push(format!("{large_files} file(s) exceed 2 MB"));
    }

    let manifests = resolve_dependency_counts(root, manifests);

    Analysis {
        files,
        languages,
        modules,
        manifests,
        todos,
        todo_total,
        todo_tags: todo_tags_map,
        tools,
        alerts,
        git,
        history,
        lockfile_ok,
        truncated,
        scanned_ms: started.elapsed().as_millis(),
        scanned_at: format_clock_utc(now_secs),
        total_lines,
        source_lines,
        test_lines,
        total_bytes,
    }
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m, d)
}

fn month_short(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "Jan",
    }
}

fn monday_of_day(day: i64) -> i64 {
    day - (day + 3).rem_euclid(7)
}

fn format_clock_utc(epoch_secs: i64) -> String {
    let secs = epoch_secs.max(0) as u64;
    let hours = (secs / 3600) % 24;
    let minutes = (secs / 60) % 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02} UTC")
}

fn git_history(root: &Path, now_secs: i64) -> GitHistory {
    let since = now_secs.saturating_sub(380 * 86_400).max(0);
    let since_arg = format!("--since={since}");
    let recent = command_output(
        root,
        "git",
        &[
            "log",
            &since_arg,
            "--max-count=3000",
            "--no-renames",
            "--numstat",
            "--format=@@@%ct",
        ],
    )
    .filter(|out| out.status.success())
    .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
    .unwrap_or_default();

    if recent.contains("@@@") {
        return parse_git_history(&recent, now_secs, false);
    }

    // Dormant repository fallback: anchor window to the most recent commit.
    let historical = command_output(
        root,
        "git",
        &[
            "log",
            "--max-count=800",
            "--no-renames",
            "--numstat",
            "--format=@@@%ct",
        ],
    )
    .filter(|out| out.status.success())
    .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
    .unwrap_or_default();

    let latest_ts = historical
        .lines()
        .find_map(|line| line.strip_prefix("@@@")?.trim().parse::<i64>().ok());
    if let Some(anchor) = latest_ts {
        parse_git_history(&historical, anchor, true)
    } else {
        parse_git_history("", now_secs, false)
    }
}

fn parse_git_history(raw: &str, anchor_secs: i64, dormant_anchor: bool) -> GitHistory {
    let mut history = GitHistory::default();
    let anchor_day = anchor_secs.div_euclid(86_400);
    let current_monday = monday_of_day(anchor_day);
    let calendar_start = current_monday - (CALENDAR_WEEKS as i64 - 1) * 7;
    let churn_start = current_monday - (CHURN_WEEKS as i64 - 1) * 7;

    for col in 0..CALENDAR_WEEKS {
        let week_end_day = calendar_start + (col as i64) * 7 + 6;
        let (_, month, _) = civil_from_days(week_end_day);
        history.month_columns[col] = month_short(month).to_string();
    }
    for col in 0..CHURN_WEEKS {
        let week_monday = churn_start + (col as i64) * 7;
        let (_, month, day) = civil_from_days(week_monday);
        history.churn_labels[col] = format!("{} {:02}", month_short(month), day);
    }
    if dormant_anchor {
        let (year, month, day) = civil_from_days(anchor_day);
        history.window_end_label = Some(format!("{year:04}-{month:02}-{day:02}"));
    }

    let cutoff_90d = anchor_secs - 90 * 86_400;
    let cutoff_30d = anchor_secs - 30 * 86_400;

    let mut current_ts: Option<i64> = None;
    let mut active_day_set: HashSet<i64> = HashSet::new();
    let mut seen_files_in_commit: HashSet<String> = HashSet::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("@@@") {
            seen_files_in_commit.clear();
            current_ts = rest.trim().parse::<i64>().ok();
            if let Some(ts) = current_ts {
                let commit_day = ts.div_euclid(86_400);
                let offset = commit_day - calendar_start;
                if (0..(CALENDAR_WEEKS as i64 * 7)).contains(&offset) {
                    let col = (offset / 7) as usize;
                    let row = (offset % 7) as usize;
                    history.calendar[row][col] += 1.0;
                    history.window_commits += 1;
                    active_day_set.insert(commit_day);
                }
            }
            continue;
        }

        let Some(ts) = current_ts else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(3, '\t');
        let (Some(add_str), Some(del_str), Some(path_raw)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // Binary files appear as "-\t-\tpath" in git --numstat.
        let added = add_str.parse::<usize>().unwrap_or(0);
        let deleted = del_str.parse::<usize>().unwrap_or(0);
        let path = path_raw.replace('\\', "/");

        if is_lockfile_or_ignored_churn(&path) {
            continue;
        }

        let commit_day = ts.div_euclid(86_400);
        let churn_offset = commit_day - churn_start;
        if (0..(CHURN_WEEKS as i64 * 7)).contains(&churn_offset) {
            let week = (churn_offset / 7) as usize;
            history.weekly_added[week] += added as f64;
            history.weekly_deleted[week] += deleted as f64;
            history.total_added_16w += added;
            history.total_deleted_16w += deleted;
        }

        if ts >= cutoff_90d {
            if seen_files_in_commit.insert(path.clone()) {
                *history.file_commits_90d.entry(path.clone()).or_default() += 1;
            }
            *history.file_churn_90d.entry(path.clone()).or_default() += added + deleted;
        }
        if ts >= cutoff_30d {
            *history.file_added_30d.entry(path.clone()).or_default() += added;
            *history.file_deleted_30d.entry(path).or_default() += deleted;
        }
    }

    history.active_days = active_day_set.len();
    history
}

fn manifest_kind(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "Cargo.toml" => Some("Rust"),
        "package.json" => Some("Node"),
        "go.mod" => Some("Go"),
        "pyproject.toml" | "requirements.txt" | "setup.py" | "Pipfile" => Some("Python"),
        "Gemfile" => Some("Ruby"),
        "composer.json" => Some("PHP"),
        "pom.xml" => Some("Java"),
        "build.gradle" | "build.gradle.kts" => Some("Java/Kotlin"),
        "CMakeLists.txt" => Some("C/C++"),
        "Makefile" | "makefile" => Some("Make"),
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" => {
            Some("Docker")
        }
        _ => None,
    }
}

fn toml_dep_keys(content: &str) -> HashSet<String> {
    let mut keys = HashSet::new();
    let mut active = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let header = trimmed.trim_matches(['[', ']']).trim();
            active = matches!(
                header.split('.').next_back().unwrap_or(""),
                "dependencies" | "dev-dependencies" | "build-dependencies"
            );
            continue;
        }
        if active && !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some((key, _)) = trimmed.split_once('=') {
                keys.insert(key.trim().trim_matches('"').to_ascii_lowercase());
            }
        }
    }
    keys
}

fn detect_frameworks(ecosystem: &str, content: &str) -> Vec<String> {
    let mut found = Vec::new();
    match ecosystem {
        "Rust" => {
            let keys = toml_dep_keys(content);
            for (dep, label) in [
                ("gpui", "⚡ GPUI"),
                ("tauri", "🪟 Tauri"),
                ("tokio", "⚙ Tokio"),
                ("axum", "🌐 Axum"),
                ("actix-web", "🌐 Actix"),
                ("bevy", "🎮 Bevy"),
            ] {
                if keys.contains(dep) {
                    found.push(label.to_string());
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
        }
        "Node" => {
            if let Ok(value) = serde_json::from_str::<Value>(content) {
                let has_dep = |name: &str| {
                    value["dependencies"].get(name).is_some()
                        || value["devDependencies"].get(name).is_some()
                };
                for (dep, label) in [
                    ("next", "▲ Next.js"),
                    ("react", "⚛ React"),
                    ("vue", "🟩 Vue"),
                    ("svelte", "🟧 Svelte"),
                    ("vite", "⚡ Vite"),
                    ("electron", "🖥 Electron"),
                    ("@tauri-apps/api", "🪟 Tauri"),
                    ("tailwindcss", "🎨 Tailwind"),
                ] {
                    if has_dep(dep) {
                        found.push(label.to_string());
                        if found.len() >= 2 {
                            break;
                        }
                    }
                }
            }
        }
        "Python" => {
            let lower = content.to_ascii_lowercase();
            for (needle, label) in [
                ("fastapi", "⚡ FastAPI"),
                ("django", "🟩 Django"),
                ("flask", "🧪 Flask"),
                ("torch", "🔥 PyTorch"),
                ("pytest", "🧪 Pytest"),
            ] {
                if lower.contains(needle) {
                    found.push(label.to_string());
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
        }
        "Go" => {
            for (needle, label) in [
                ("gin-gonic/gin", "🍸 Gin"),
                ("gofiber/fiber", "⚡ Fiber"),
                ("spf13/cobra", "🐍 Cobra"),
                ("charmbracelet/bubbletea", "🧋 BubbleTea"),
            ] {
                if content.contains(needle) {
                    found.push(label.to_string());
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
        }
        _ => {}
    }
    found
}

fn read_manifest(root: &Path, path: &str) -> Manifest {
    let ecosystem = manifest_kind(path).unwrap_or("Other").to_string();
    let content = read_limited(&root.join(path), 256 * 1024)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let frameworks = detect_frameworks(&ecosystem, &content);
    let mut manifest = Manifest {
        path: path.to_string(),
        ecosystem: ecosystem.clone(),
        name: String::new(),
        frameworks,
        deps: None,
        resolved: None,
    };
    match ecosystem.as_str() {
        "Node" => {
            if let Ok(value) = serde_json::from_str::<Value>(&content) {
                manifest.name = value["name"].as_str().unwrap_or("").to_string();
                let runtime = value["dependencies"].as_object().map_or(0, |map| map.len());
                let dev = value["devDependencies"]
                    .as_object()
                    .map_or(0, |map| map.len());
                manifest.deps = Some(runtime + dev);
            }
        }
        "Rust" => {
            manifest.name = toml_value(&content, "name");
            if manifest.name.is_empty() && content.contains("[workspace]") {
                manifest.name = "workspace".into();
            }
            manifest.deps = Some(count_sections(
                &content,
                &[
                    "dependencies",
                    "dev-dependencies",
                    "build-dependencies",
                    "workspace.dependencies",
                ],
            ));
        }
        "Go" => {
            manifest.name = content
                .lines()
                .find(|line| line.trim_start().starts_with("module "))
                .map(|line| line.trim_start()[7..].trim().to_string())
                .unwrap_or_default();
            let deps = content
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    !trimmed.is_empty()
                        && !trimmed.starts_with("//")
                        && !trimmed.starts_with("module ")
                        && !trimmed.starts_with("go ")
                        && !trimmed.starts_with("toolchain ")
                        && trimmed != ")"
                        && !trimmed.starts_with("require (")
                        && (trimmed.contains(' ') && trimmed.contains('/')
                            || trimmed.starts_with("require "))
                })
                .count();
            manifest.deps = Some(deps);
        }
        "Python" => {
            manifest.name = toml_value(&content, "name");
            if path.ends_with("requirements.txt") {
                let deps = content
                    .lines()
                    .filter(|line| {
                        let line = line.trim();
                        !line.is_empty() && !line.starts_with('#') && !line.starts_with('-')
                    })
                    .count();
                manifest.deps = Some(deps);
            } else {
                manifest.deps = Some(count_sections(
                    &content,
                    &["dependencies", "dev-dependencies"],
                ));
            }
        }
        "Ruby" => {
            manifest.deps = Some(
                content
                    .lines()
                    .filter(|line| line.trim_start().starts_with("gem "))
                    .count(),
            );
        }
        "PHP" => {
            if let Ok(value) = serde_json::from_str::<Value>(&content) {
                manifest.name = value["name"].as_str().unwrap_or("").to_string();
                manifest.deps = Some(
                    value["require"].as_object().map_or(0, |map| map.len())
                        + value["require-dev"].as_object().map_or(0, |map| map.len()),
                );
            }
        }
        "Java" => {
            manifest.deps = Some(content.matches("<dependency>").count());
        }
        "Java/Kotlin" => {
            manifest.deps = Some(
                content
                    .lines()
                    .filter(|line| {
                        let line = line.trim();
                        line.starts_with("implementation")
                            || line.starts_with("api ")
                            || line.starts_with("compile")
                    })
                    .count(),
            );
        }
        "C/C++" => {
            manifest.name = content
                .lines()
                .find(|line| line.trim_start().starts_with("project("))
                .map(|line| {
                    line.trim()
                        .trim_start_matches("project(")
                        .trim_end_matches(')')
                        .to_string()
                })
                .unwrap_or_default();
        }
        _ => {}
    }
    if manifest.name.trim().is_empty() {
        manifest.name = path
            .rsplit('/')
            .nth(1)
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path))
            .to_string();
    }
    manifest
}

fn toml_value(content: &str, key: &str) -> String {
    content
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let rest = trimmed.strip_prefix(key)?.trim_start();
            let rest = rest.strip_prefix('=')?.trim();
            if rest.starts_with('"') {
                Some(rest.trim_matches('"').to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn count_sections(content: &str, sections: &[&str]) -> usize {
    let mut active = false;
    let mut count = 0usize;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let header = trimmed.trim_matches(['[', ']']).trim();
            active = sections.iter().any(|section| {
                header == *section
                    || header
                        .split('.')
                        .next_back()
                        .is_some_and(|name| name == *section)
            });
            continue;
        }
        if active && !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with('[') {
            count += 1;
        }
    }
    count
}

fn resolve_dependency_counts(root: &Path, mut manifests: Vec<Manifest>) -> Vec<Manifest> {
    let mut resolved = 0usize;
    let mut ecosystems = BTreeSet::new();
    let mut order = (0..manifests.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        (
            Path::new(&manifests[*index].path).components().count(),
            manifests[*index].path.clone(),
        )
    });
    for index in order {
        if resolved >= 4 {
            break;
        }
        let manifest = &mut manifests[index];
        if manifest.resolved.is_some() || ecosystems.contains(&manifest.ecosystem) {
            continue;
        }
        let directory = root.join(
            Path::new(&manifest.path)
                .parent()
                .unwrap_or_else(|| Path::new("")),
        );
        if !directory.is_dir() {
            continue;
        }
        if let Some(count) = resolve_manifest(manifest, &directory) {
            manifest.resolved = Some(count);
            ecosystems.insert(manifest.ecosystem.clone());
            resolved += 1;
        }
    }
    manifests
}

fn resolve_manifest(manifest: &Manifest, directory: &Path) -> Option<usize> {
    match manifest.ecosystem.as_str() {
        "Node" => {
            if !directory.join("package-lock.json").is_file()
                && !directory.join("npm-shrinkwrap.json").is_file()
            {
                return None;
            }
            let output = command_output(directory, "npm", &["ls", "--all", "--json", "--silent"])?;
            let value: Value = serde_json::from_slice(&output.stdout).ok()?;
            Some(npm_dependency_count(&value))
        }
        "Rust" => {
            let output = command_output(
                directory,
                "cargo",
                &["metadata", "--format-version", "1", "--locked"],
            )?;
            let value: Value = serde_json::from_slice(&output.stdout).ok()?;
            Some(value["resolve"]["nodes"].as_array().map_or(0, Vec::len))
        }
        "Go" => {
            let output = command_output(directory, "go", &["list", "-m", "all"])?;
            let text = String::from_utf8_lossy(&output.stdout);
            Some(
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .skip(1)
                    .count(),
            )
        }
        "Python" if directory.join("poetry.lock").is_file() => {
            let output = command_output(directory, "poetry", &["show", "--format=json"])?;
            let value: Value = serde_json::from_slice(&output.stdout).ok()?;
            Some(value.as_array().map_or(0, Vec::len))
        }
        "PHP" if directory.join("composer.lock").is_file() => {
            let output = command_output(directory, "composer", &["show", "--format=json"])?;
            let value: Value = serde_json::from_slice(&output.stdout).ok()?;
            Some(value["installed"].as_array().map_or(0, Vec::len))
        }
        "Ruby" if directory.join("Gemfile.lock").is_file() => {
            let output = command_output(directory, "bundle", &["list"])?;
            let text = String::from_utf8_lossy(&output.stdout);
            Some(
                text.lines()
                    .filter(|line| line.trim_start().starts_with("* "))
                    .count(),
            )
        }
        _ => None,
    }
}

fn npm_dependency_count(value: &Value) -> usize {
    fn walk_deps(value: &Value, seen: &mut BTreeSet<String>) {
        if let Some(dependencies) = value["dependencies"].as_object() {
            for (name, dependency) in dependencies {
                seen.insert(name.clone());
                walk_deps(dependency, seen);
            }
        }
    }
    let mut seen = BTreeSet::new();
    walk_deps(value, &mut seen);
    seen.len()
}

fn command_output(directory: &Path, program: &str, args: &[&str]) -> Option<std::process::Output> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().ok()? {
            let stdout = receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap_or_default();
            return Some(std::process::Output {
                status,
                stdout,
                stderr: Vec::new(),
            });
        }
        if started.elapsed() > Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn toolchains(manifests: &[Manifest]) -> Vec<Tool> {
    let ecosystems: BTreeSet<&str> = manifests.iter().map(|m| m.ecosystem.as_str()).collect();
    let mut tools = vec![probe_tool("git", "Git", &["--version"])];
    if ecosystems.contains("Rust") {
        tools.push(probe_tool("cargo", "Rust", &["--version"]));
    }
    if ecosystems.contains("Node") {
        tools.push(probe_tool("node", "Node", &["--version"]));
    }
    if ecosystems.contains("Go") {
        tools.push(probe_tool("go", "Go", &["version"]));
    }
    if ecosystems.contains("Python") {
        tools.push(probe_tool("python3", "Python", &["--version"]));
    }
    if ecosystems.contains("Java") || ecosystems.contains("Java/Kotlin") {
        tools.push(probe_tool("java", "Java", &["-version"]));
    }
    if ecosystems.contains("Docker") {
        tools.push(probe_tool("docker", "Docker", &["--version"]));
    }
    tools
}

fn compact_tool_version(name: &str, raw: &str) -> String {
    let line = raw.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return name.to_string();
    }
    // Extract compact token such as "cargo 1.86.0", "node v22.14.0", "go1.24.1".
    let tokens: Vec<&str> = line.split_whitespace().collect();
    match name {
        "git" if tokens.len() >= 3 => format!("git {}", tokens[2]),
        "cargo" if tokens.len() >= 2 => format!("cargo {}", tokens[1]),
        "node" => format!("node {}", tokens[0]),
        "go" if tokens.len() >= 3 => tokens[2].replacen("go", "go ", 1),
        "python3" if tokens.len() >= 2 => format!("python {}", tokens[1]),
        "docker" if tokens.len() >= 3 => {
            format!("docker {}", tokens[2].trim_end_matches(','))
        }
        _ => line.chars().take(28).collect(),
    }
}

fn probe_tool(name: &str, ecosystem: &str, args: &[&str]) -> Tool {
    match Command::new(name).args(args).output() {
        Ok(output) => {
            let raw = if output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stderr).to_string()
            } else {
                String::from_utf8_lossy(&output.stdout).to_string()
            };
            Tool {
                name: name.into(),
                ecosystem: ecosystem.into(),
                available: output.status.success(),
                version: if output.status.success() {
                    compact_tool_version(name, &raw)
                } else {
                    "missing".into()
                },
            }
        }
        Err(_) => Tool {
            name: name.into(),
            ecosystem: ecosystem.into(),
            available: false,
            version: "missing".into(),
        },
    }
}

fn git_info(root: &Path) -> GitInfo {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output();
    let Ok(output) = output else {
        return GitInfo {
            error: "git is not installed".into(),
            ..Default::default()
        };
    };
    if !output.status.success() {
        return GitInfo {
            error: "not a git repository".into(),
            ..Default::default()
        };
    }
    let branch = command_line(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|| "detached".into());
    let last_commit =
        command_line(root, &["log", "-1", "--format=%h (%cr)"]).unwrap_or_default();
    let dirty = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("status")
        .arg("--porcelain")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).lines().count())
        .unwrap_or(0);
    GitInfo {
        repo: true,
        branch,
        dirty,
        last_commit,
        error: String::new(),
    }
}

fn command_line(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn text(value: impl Into<String>, size: &str, color: &str) -> Value {
    json!({
        "kind": "text",
        "text": value.into().replace('\0', ""),
        "size": size,
        "color": color
    })
}

fn mono_text(value: impl Into<String>, size: &str, color: &str) -> Value {
    json!({
        "kind": "text",
        "text": value.into().replace('\0', ""),
        "size": size,
        "color": color,
        "monospace": true
    })
}

fn card(title: &str, children: Vec<Value>) -> Value {
    let mut all = vec![text(title, "subtitle", "ink")];
    all.extend(children);
    json!({"kind": "card", "gap": 10, "children": all})
}

fn table_weighted(columns: &[&str], rows: Vec<Vec<String>>, weights: &[f32]) -> Value {
    let mut node = json!({
        "kind": "table",
        "columns": columns,
        "rows": rows.into_iter()
            .take(250)
            .map(|row| row.into_iter().take(24).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    });
    if weights.len() == columns.len() {
        node["weights"] = json!(weights);
    }
    node
}

fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * part as f64 / total as f64
    }
}

fn fmt_int(value: usize) -> String {
    let digits: Vec<char> = value.to_string().chars().collect();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, ch) in digits.iter().enumerate() {
        if idx > 0 && (digits.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}

fn fmt_size(size: u64) -> String {
    if size >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if size >= 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}

fn short_axis_label(raw: &str) -> String {
    if raw.chars().count() <= 12 {
        raw.to_string()
    } else {
        let mut s: String = raw.chars().take(11).collect();
        s.push('.');
        s
    }
}

fn disambiguated_file_labels(paths: &[&str]) -> Vec<String> {
    let mut basename_counts: HashMap<&str, usize> = HashMap::new();
    for path in paths {
        let base = path.rsplit('/').next().unwrap_or(path);
        *basename_counts.entry(base).or_default() += 1;
    }
    paths
        .iter()
        .map(|path| {
            let base = path.rsplit('/').next().unwrap_or(path);
            if basename_counts.get(base).copied().unwrap_or(0) <= 1 {
                short_axis_label(base)
            } else if let Some(parent) = path.rsplit('/').nth(1) {
                let p_short: String = parent.chars().take(4).collect();
                short_axis_label(&format!("{p_short}/{base}"))
            } else {
                short_axis_label(base)
            }
        })
        .collect()
}

fn overview_tab(analysis: &Analysis) -> Value {
    let mut children = Vec::new();

    if analysis.git.repo {
        let window_desc = match &analysis.history.window_end_label {
            Some(end_date) => format!("52 weeks to {end_date}"),
            None => "last 52 weeks".to_string(),
        };
        let head_part = if analysis.git.last_commit.is_empty() {
            String::new()
        } else {
            format!(" · head {}", analysis.git.last_commit)
        };
        children.push(card(
            "Commit activity",
            vec![
                mono_text(
                    format!(
                        "{window_desc} · {} commits · {} active days{head_part}",
                        fmt_int(analysis.history.window_commits),
                        fmt_int(analysis.history.active_days)
                    ),
                    "caption",
                    "muted",
                ),
                json!({
                    "kind": "heatmap",
                    "values": analysis.history.calendar,
                    "rows": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
                    "columns": analysis.history.month_columns,
                    "color": "accent"
                }),
            ],
        ));

        children.push(card(
            "Code churn · 16 weeks",
            vec![
                mono_text(
                    format!(
                        "+{} added · -{} deleted (source & config, excludes lockfiles)",
                        fmt_int(analysis.history.total_added_16w),
                        fmt_int(analysis.history.total_deleted_16w)
                    ),
                    "caption",
                    "muted",
                ),
                json!({
                    "kind": "chart",
                    "chart_type": "area",
                    "labels": analysis.history.churn_labels,
                    "series": [
                        {
                            "label": "Added",
                            "values": analysis.history.weekly_added,
                            "color": "positive"
                        },
                        {
                            "label": "Deleted",
                            "values": analysis.history.weekly_deleted,
                            "color": "warning"
                        }
                    ],
                    "height": 185
                }),
            ],
        ));
    } else {
        children.push(mono_text(
            format!(
                "Git unavailable ({}) · showing static workspace breakdown",
                analysis.git.error
            ),
            "caption",
            "muted",
        ));
    }

    let module_rows: Vec<Vec<String>> = analysis
        .modules
        .iter()
        .take(25)
        .map(|module| {
            let churn = if !analysis.git.repo || (module.added_30d == 0 && module.deleted_30d == 0)
            {
                "-".into()
            } else {
                format!("+{} -{}", fmt_int(module.added_30d), fmt_int(module.deleted_30d))
            };
            vec![
                module.name.clone(),
                module.primary_language(),
                fmt_int(module.files),
                fmt_int(module.source_lines),
                if module.test_lines > 0 {
                    fmt_int(module.test_lines)
                } else {
                    "-".into()
                },
                churn,
            ]
        })
        .collect();

    children.push(card(
        "Modules",
        vec![
            mono_text(
                format!(
                    "{} modules · {} source LOC · {} test LOC · {}",
                    fmt_int(analysis.modules.len()),
                    fmt_int(analysis.source_lines),
                    fmt_int(analysis.test_lines),
                    fmt_size(analysis.total_bytes)
                ),
                "caption",
                "muted",
            ),
            table_weighted(
                &["Module", "Stack", "Files", "Source LOC", "Test LOC", "30d Churn"],
                module_rows,
                &[2.2, 1.3, 0.6, 0.9, 0.9, 1.3],
            ),
        ],
    ));

    if !analysis.alerts.is_empty() {
        let alert_nodes = analysis
            .alerts
            .iter()
            .take(8)
            .map(|msg| mono_text(msg, "small", "warning"))
            .collect();
        children.push(card("Alerts", alert_nodes));
    }

    json!({
        "kind": "column",
        "gap": 12,
        "children": children
    })
}

fn languages_tab(analysis: &Analysis) -> Value {
    const SLICE_COLORS: [&str; 8] = [
        "accent",
        "series2",
        "series3",
        "series4",
        "series5",
        "series6",
        "positive",
        "warning",
    ];

    let mut entries = analysis.languages.iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        b.1.lines
            .cmp(&a.1.lines)
            .then_with(|| b.1.bytes.cmp(&a.1.bytes))
    });

    let source_entries: Vec<(&String, &LanguageStats)> = entries
        .iter()
        .copied()
        .filter(|(_, stats)| stats.source && stats.lines > 0)
        .collect();
    let (chart_title, center_label, donut_pool) = if !source_entries.is_empty() {
        ("Source composition", "Source LOC", source_entries)
    } else {
        let non_empty: Vec<(&String, &LanguageStats)> = entries
            .iter()
            .copied()
            .filter(|(_, stats)| stats.lines > 0)
            .collect();
        ("Content composition", "Total LOC", non_empty)
    };

    let composition_body = if donut_pool.is_empty() {
        vec![mono_text(
            "No text files with countable lines.",
            "small",
            "muted",
        )]
    } else {
        let mut slices = Vec::new();
        let mut remainder_lines = 0usize;
        for (idx, (name, stats)) in donut_pool.iter().enumerate() {
            if idx < 7 {
                slices.push(json!({
                    "label": language_label(name),
                    "value": stats.lines as f64,
                    "color": SLICE_COLORS[idx]
                }));
            } else {
                remainder_lines += stats.lines;
            }
        }
        if remainder_lines > 0 {
            slices.push(json!({
                "label": "📦 Other",
                "value": remainder_lines as f64,
                "color": SLICE_COLORS[7]
            }));
        }
        vec![json!({
            "kind": "donut",
            "slices": slices,
            "label": center_label
        })]
    };

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|(name, stats)| {
            let avg = stats.lines.checked_div(stats.files).unwrap_or(0);
            vec![
                language_label(name),
                language_kind(name, stats.source).into(),
                fmt_int(stats.files),
                fmt_int(stats.lines),
                fmt_int(avg),
                format!("{:.1}%", percent(stats.lines, analysis.total_lines)),
                fmt_size(stats.bytes),
            ]
        })
        .collect();

    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            card(chart_title, composition_body),
            card(
                "All file types",
                vec![table_weighted(
                    &["Language", "Kind", "Files", "Lines", "Avg/File", "Share", "Size"],
                    rows,
                    &[1.7, 0.7, 0.6, 0.9, 0.8, 0.7, 0.9],
                )],
            )
        ]
    })
}

fn hotspot_score(file: &FileInfo, has_git_churn: bool) -> f64 {
    if has_git_churn {
        let size_factor = (file.lines as f64).ln_1p();
        let activity = (file.commits_90d as f64) * 14.0 + (file.churn_90d as f64).sqrt();
        size_factor * activity + (file.todo as f64) * 18.0 + (file.lines as f64) * 0.02
    } else {
        (file.lines as f64) + (file.todo as f64) * 80.0
    }
}

fn hotspots_tab(analysis: &Analysis) -> Value {
    let mut candidates: Vec<&FileInfo> = analysis
        .files
        .iter()
        .filter(|file| file.source && file.lines > 0)
        .collect();
    if candidates.is_empty() {
        candidates = analysis
            .files
            .iter()
            .filter(|file| {
                file.lines > 0 && file.language != "Lockfile" && file.language != "Other"
            })
            .collect();
    }

    let has_git_churn = candidates.iter().any(|file| file.commits_90d > 0);
    candidates.sort_by(|a, b| {
        hotspot_score(b, has_git_churn)
            .partial_cmp(&hotspot_score(a, has_git_churn))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.lines.cmp(&a.lines))
    });

    let mut children = Vec::new();
    let top_chart: Vec<&FileInfo> = candidates.iter().copied().take(10).collect();
    if !top_chart.is_empty() {
        let paths: Vec<&str> = top_chart.iter().map(|f| f.path.as_str()).collect();
        let labels = disambiguated_file_labels(&paths);
        let (chart_title, series_label, series_color, values): (&str, &str, &str, Vec<f64>) =
            if has_git_churn {
                (
                    "Top hotspots · 90d lines changed",
                    "90d churn",
                    "warning",
                    top_chart
                        .iter()
                        .map(|f| f.churn_90d.max(f.commits_90d) as f64)
                        .collect(),
                )
            } else {
                (
                    "Top files by LOC",
                    "Lines",
                    "accent",
                    top_chart.iter().map(|f| f.lines as f64).collect(),
                )
            };
        children.push(card(
            chart_title,
            vec![json!({
                "kind": "chart",
                "chart_type": "bar",
                "labels": labels,
                "series": [{
                    "label": series_label,
                    "values": values,
                    "color": series_color
                }],
                "height": 195
            })],
        ));
    }

    let ranking_note = if has_git_churn {
        "Ranked by 90d commit frequency x file size · top 80 files"
    } else {
        "Ranked by line count and inline markers · top 80 files"
    };

    let rows: Vec<Vec<String>> = candidates
        .iter()
        .take(80)
        .map(|file| {
            vec![
                file.path.clone(),
                language_label(&file.language),
                fmt_int(file.lines),
                if file.commits_90d > 0 {
                    fmt_int(file.commits_90d)
                } else {
                    "-".into()
                },
                if file.churn_90d > 0 {
                    fmt_int(file.churn_90d)
                } else {
                    "-".into()
                },
                if file.todo > 0 {
                    fmt_int(file.todo)
                } else {
                    "-".into()
                },
            ]
        })
        .collect();

    children.push(card(
        "File hotspots",
        vec![
            mono_text(ranking_note, "caption", "muted"),
            table_weighted(
                &[
                    "Path",
                    "Language",
                    "LOC",
                    "90d Commits",
                    "90d Churn",
                    "Markers",
                ],
                rows,
                &[3.1, 1.3, 0.8, 0.9, 0.9, 0.7],
            ),
        ],
    ));

    json!({
        "kind": "column",
        "gap": 12,
        "children": children
    })
}

fn tool_for_ecosystem<'a>(tools: &'a [Tool], ecosystem: &str) -> Option<&'a Tool> {
    let target = match ecosystem {
        "Java/Kotlin" => "Java",
        other => other,
    };
    tools.iter().find(|tool| tool.ecosystem == target)
}

fn short_package_labels(names: &[&str]) -> Vec<String> {
    let unscoped: Vec<&str> = names
        .iter()
        .map(|name| name.rsplit('/').next().unwrap_or(name))
        .collect();
    let mut prefix_counts: HashMap<&str, usize> = HashMap::new();
    for name in &unscoped {
        if let Some((prefix, _)) = name.split_once('-') {
            if prefix.len() >= 3 {
                *prefix_counts.entry(prefix).or_default() += 1;
            }
        }
    }
    let common_prefix = prefix_counts
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .max_by_key(|(_, count)| *count)
        .map(|(prefix, _)| prefix);

    unscoped
        .into_iter()
        .map(|name| {
            if let Some(prefix) = common_prefix {
                if name.len() > 12 {
                    if let Some(stripped) = name.strip_prefix(&format!("{prefix}-")) {
                        if !stripped.is_empty() {
                            return short_axis_label(stripped);
                        }
                    }
                }
            }
            short_axis_label(name)
        })
        .collect()
}

fn dependencies_tab(analysis: &Analysis) -> Value {
    let mut children = Vec::new();

    let mut dep_manifests: Vec<&Manifest> = analysis
        .manifests
        .iter()
        .filter(|m| m.deps.unwrap_or(0) > 0)
        .collect();
    dep_manifests.sort_by_key(|m| std::cmp::Reverse(m.deps.unwrap_or(0)));

    if !dep_manifests.is_empty() {
        let top: Vec<&Manifest> = dep_manifests.into_iter().take(10).collect();
        let names: Vec<&str> = top.iter().map(|m| m.name.as_str()).collect();
        let labels = short_package_labels(&names);
        let values: Vec<f64> = top
            .iter()
            .map(|m| m.deps.unwrap_or(0) as f64)
            .collect();
        children.push(card(
            "Declared dependencies by package",
            vec![json!({
                "kind": "chart",
                "chart_type": "bar",
                "labels": labels,
                "series": [{
                    "label": "Declared deps",
                    "values": values,
                    "color": "series2"
                }],
                "height": 185
            })],
        ));
    }

    let tool_strip = analysis
        .tools
        .iter()
        .map(|tool| {
            if tool.available {
                tool.version.clone()
            } else {
                format!("{}: missing", tool.name)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let lock_note = if analysis.lockfile_ok {
        "lockfiles: ok"
    } else {
        "lockfiles: missing"
    };

    if analysis.manifests.is_empty() {
        children.push(card(
            "Manifests & toolchains",
            vec![
                mono_text(format!("{tool_strip} · {lock_note}"), "caption", "muted"),
                mono_text("No package or build manifests found.", "small", "muted"),
            ],
        ));
    } else {
        let rows: Vec<Vec<String>> = analysis
            .manifests
            .iter()
            .map(|manifest| {
                let pkg = if manifest.frameworks.is_empty() {
                    manifest.name.clone()
                } else {
                    format!("{} ({})", manifest.name, manifest.frameworks.join(", "))
                };
                let toolchain = tool_for_ecosystem(&analysis.tools, &manifest.ecosystem)
                    .map(|tool| tool.version.clone())
                    .unwrap_or_else(|| "-".into());
                vec![
                    manifest.path.clone(),
                    ecosystem_label(&manifest.ecosystem),
                    pkg,
                    manifest
                        .deps
                        .map(fmt_int)
                        .unwrap_or_else(|| "-".into()),
                    manifest
                        .resolved
                        .map(fmt_int)
                        .unwrap_or_else(|| "-".into()),
                    toolchain,
                ]
            })
            .collect();

        children.push(card(
            "Manifests & toolchains",
            vec![
                mono_text(format!("{tool_strip} · {lock_note}"), "caption", "muted"),
                table_weighted(
                    &[
                        "Manifest",
                        "Ecosystem",
                        "Package",
                        "Declared",
                        "Resolved",
                        "Toolchain",
                    ],
                    rows,
                    &[2.1, 1.1, 1.8, 0.8, 0.8, 1.3],
                ),
            ],
        ));
    }

    json!({
        "kind": "column",
        "gap": 12,
        "children": children
    })
}

fn todo_tab(analysis: &Analysis) -> Value {
    let files_with_todos = analysis.files.iter().filter(|f| f.todo > 0).count();
    let tag_summary = ["TODO", "FIXME", "HACK", "XXX"]
        .iter()
        .map(|tag| {
            let count = analysis.todo_tags.get(*tag).copied().unwrap_or(0);
            format!("{tag} {}", fmt_int(count))
        })
        .collect::<Vec<_>>()
        .join(" · ");

    if analysis.todo_total == 0 {
        return json!({
            "kind": "column",
            "gap": 12,
            "children": [
                card(
                    "Code markers",
                    vec![mono_text(
                        "clean · 0 TODO, FIXME, HACK, or XXX markers across workspace",
                        "small",
                        "positive"
                    )]
                )
            ]
        });
    }

    let mut children = Vec::new();
    let mut mod_todos: Vec<&ModuleStats> =
        analysis.modules.iter().filter(|m| m.todos > 0).collect();
    mod_todos.sort_by_key(|m| std::cmp::Reverse(m.todos));

    if !mod_todos.is_empty() {
        let top: Vec<&ModuleStats> = mod_todos.into_iter().take(10).collect();
        let labels: Vec<String> = top
            .iter()
            .map(|m| {
                let short = m.name.rsplit('/').next().unwrap_or(&m.name);
                short_axis_label(short)
            })
            .collect();
        let values: Vec<f64> = top.iter().map(|m| m.todos as f64).collect();
        children.push(card(
            "Markers by module",
            vec![json!({
                "kind": "chart",
                "chart_type": "bar",
                "labels": labels,
                "series": [{
                    "label": "Markers",
                    "values": values,
                    "color": "warning"
                }],
                "height": 175
            })],
        ));
    }

    let rows: Vec<Vec<String>> = analysis
        .todos
        .iter()
        .take(150)
        .map(|todo| {
            vec![
                todo.path.clone(),
                format!("L{:03}", todo.line),
                todo.tag.clone(),
                todo.text.clone(),
            ]
        })
        .collect();

    children.push(card(
        "Marker hits",
        vec![
            mono_text(
                format!(
                    "{} markers across {} files · {tag_summary}",
                    fmt_int(analysis.todo_total),
                    fmt_int(files_with_todos)
                ),
                "caption",
                "muted",
            ),
            table_weighted(
                &["Path", "Line", "Tag", "Excerpt"],
                rows,
                &[2.5, 0.6, 0.7, 3.4],
            ),
        ],
    ));

    json!({
        "kind": "column",
        "gap": 12,
        "children": children
    })
}

fn tree(state: &State) -> Value {
    let body = if let Some(analysis) = &state.analysis {
        match state.tab {
            0 => overview_tab(analysis),
            1 => languages_tab(analysis),
            2 => hotspots_tab(analysis),
            3 => dependencies_tab(analysis),
            _ => todo_tab(analysis),
        }
    } else {
        json!({
            "kind": "column",
            "gap": 12,
            "children": [mono_text(state.status.clone(), "small", "muted")]
        })
    };

    json!({
        "kind": "page_tabs",
        "labels": ["Overview", "Languages", "Hotspots", "Dependencies", "TODO"],
        "selected": state.tab,
        "handler": "tab",
        "nav_width": 180,
        "children": [{
            "kind": "column",
            "gap": 12,
            "children": [
                {
                    "kind": "row",
                    "gap": 8,
                    "children": [
                        mono_text(state.status.clone(), "caption", "muted"),
                        {"kind": "spacer"},
                        {"kind": "button", "label": "Rescan", "handler": "refresh", "disabled": false}
                    ]
                },
                body
            ]
        }]
    })
}

fn publish(out: &mut impl Write, state: &State) {
    let line = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": "ui.set",
        "params": {"surface": SURFACE, "tree": tree(state)}
    }))
    .expect("workspace analysis message should encode");
    writeln!(out, "{line}").expect("workspace analysis stdout should be writable");
    out.flush().expect("workspace analysis stdout should flush");
}

fn write_line(out: &mut impl Write, value: Value) {
    let line = serde_json::to_string(&value).expect("workspace analysis message should encode");
    writeln!(out, "{line}").expect("workspace analysis stdout should be writable");
    out.flush().expect("workspace analysis stdout should flush");
}

fn main() {
    let (sender, receiver) = mpsc::channel::<Value>();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        }
    });

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut state = State::new();
    let mut next_request_id = 2u64;
    let mut pending_workspace_ids: HashSet<u64> = HashSet::new();

    let mut request_workspace = |out: &mut BufWriter<_>, pending: &mut HashSet<u64>| {
        let id = next_request_id;
        next_request_id += 1;
        pending.insert(id);
        write_line(
            out,
            json!({"jsonrpc": "2.0", "id": id, "method": "workspace.info", "params": {}}),
        );
    };

    loop {
        match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(message) => {
                let method = message["method"].as_str();
                if method.is_none() {
                    if let Some(id) = message["id"].as_u64() {
                        if pending_workspace_ids.remove(&id) {
                            if let Some(root) = message["result"]["root"].as_str() {
                                if state.set_workspace_root(PathBuf::from(root)) {
                                    publish(&mut out, &state);
                                }
                            }
                        }
                    }
                    continue;
                }
                match method.unwrap_or_default() {
                    "initialize" => {
                        write_line(
                            &mut out,
                            json!({
                                "jsonrpc": "2.0",
                                "id": message["id"],
                                "result": {
                                    "protocol": 1,
                                    "ui": [{
                                        "id": SURFACE,
                                        "title": "Workspace Analysis",
                                        "icon": "search",
                                        "order": 2
                                    }]
                                }
                            }),
                        );
                        publish(&mut out, &state);
                        request_workspace(&mut out, &mut pending_workspace_ids);
                    }
                    "ui.open" => {
                        if state.analysis.is_none() && state.root.is_some() {
                            state.rescan();
                        }
                        publish(&mut out, &state);
                        request_workspace(&mut out, &mut pending_workspace_ids);
                    }
                    "ui.event" => {
                        if message["params"]["surface"].as_str() != Some(SURFACE) {
                            continue;
                        }
                        state.handle_event(
                            message["params"]["handler"].as_str().unwrap_or_default(),
                            &message["params"]["value"],
                        );
                        publish(&mut out, &state);
                    }
                    "shutdown" => break,
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_calendar_math_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        // 1970-01-01 was Thursday; Monday of that week was 3 days earlier.
        assert_eq!(monday_of_day(0), -3);
        assert_eq!(monday_of_day(4), 4);
    }

    #[test]
    fn marker_scanner_ignores_identifiers_and_string_literals() {
        assert_eq!(find_marker_tag("const MAX_TODOS: usize = 200;"), None);
        assert_eq!(
            find_marker_tag("let tags = [\"TODO\", \"FIXME\", \"HACK\"];"),
            None
        );
        assert_eq!(
            find_marker_tag("assert_eq!(find_marker_tag(\"// TODO: fake\"), None);"),
            None
        );
        assert_eq!(
            find_marker_tag("let icon = \"🦀\"; // TODO: tighten timeout handling"),
            Some("TODO")
        );
        assert_eq!(find_marker_tag("let trailing = \"abc\\"), None);
        assert_eq!(
            find_marker_tag("# FIXME(worker) race on reconnect"),
            Some("FIXME")
        );
        assert_eq!(find_marker_tag("- TODO: document protocol"), Some("TODO"));
    }

    #[test]
    fn git_history_parser_buckets_commits_and_skips_lockfiles() {
        let anchor = 1_762_000_000i64;
        let raw = format!(
            "@@@{anchor}\n120\t30\tcrates/app/src/main.rs\n9000\t8000\tCargo.lock\n@@@{}\n15\t5\tcrates/app/src/main.rs\n",
            anchor - 2 * 86_400
        );
        let parsed = parse_git_history(&raw, anchor, false);
        assert_eq!(parsed.window_commits, 2);
        assert_eq!(parsed.active_days, 2);
        assert_eq!(parsed.total_added_16w, 135);
        assert_eq!(parsed.total_deleted_16w, 35);
        assert_eq!(
            parsed.file_commits_90d.get("crates/app/src/main.rs"),
            Some(&2)
        );
        assert_eq!(
            parsed.file_churn_90d.get("crates/app/src/main.rs"),
            Some(&170)
        );
        assert!(!parsed.file_churn_90d.contains_key("Cargo.lock"));
    }

    #[test]
    fn module_grouping_adapts_to_monorepo_and_single_project() {
        assert_eq!(
            module_for("crates/monocode-gpui/src/visuals.rs", false),
            "crates/monocode-gpui"
        );
        assert_eq!(
            module_for("plugin-sources/dashboard/main.py", false),
            "plugin-sources/dashboard"
        );
        assert_eq!(module_for("plugins/PROTOCOL.md", false), "plugins");
        assert_eq!(module_for("Cargo.toml", false), ".");
        assert_eq!(module_for("src/visuals/charts.rs", true), "src/visuals");
    }

    #[test]
    fn rendered_tabs_preserve_emojis_and_avoid_em_dashes() {
        let mut analysis = Analysis {
            git: GitInfo {
                repo: true,
                branch: "main".into(),
                dirty: 0,
                last_commit: "a1b2c3d (1h ago)".into(),
                error: String::new(),
            },
            history: parse_git_history("@@@1762000000\n40\t10\tsrc/lib.rs\n", 1_762_000_000, false),
            total_lines: 320,
            source_lines: 280,
            test_lines: 40,
            total_bytes: 12_400,
            lockfile_ok: true,
            scanned_ms: 14,
            scanned_at: "12:00:00 UTC".into(),
            ..Default::default()
        };
        analysis.languages.insert(
            "Rust".into(),
            LanguageStats {
                files: 2,
                lines: 320,
                bytes: 12_400,
                source: true,
            },
        );
        analysis.files.push(FileInfo {
            path: "src/lib.rs".into(),
            size: 12_400,
            lines: 320,
            source_lines: 280,
            test_lines: 40,
            language: "Rust".into(),
            source: true,
            todo: 1,
            commits_90d: 3,
            churn_90d: 50,
            ..Default::default()
        });
        let mut lang_lines = BTreeMap::new();
        lang_lines.insert("Rust".into(), 320);
        analysis.modules.push(ModuleStats {
            name: "src".into(),
            files: 2,
            source_lines: 280,
            test_lines: 40,
            todos: 1,
            added_30d: 40,
            deleted_30d: 10,
            lang_lines,
        });
        analysis.manifests.push(Manifest {
            path: "Cargo.toml".into(),
            ecosystem: "Rust".into(),
            name: "demo".into(),
            frameworks: vec!["⚡ GPUI".into()],
            deps: Some(4),
            resolved: Some(28),
        });
        analysis.tools.push(Tool {
            name: "cargo".into(),
            ecosystem: "Rust".into(),
            available: true,
            version: "cargo 1.86.0".into(),
        });
        analysis.todo_total = 1;
        analysis.todo_tags.insert("TODO".into(), 1);
        analysis.todos.push(Todo {
            path: "src/lib.rs".into(),
            line: 17,
            tag: "TODO".into(),
            text: "// TODO: add cache".into(),
        });

        let mut state = State::new();
        state.status = status_line(&analysis);
        state.analysis = Some(analysis);

        for tab in 0..=4 {
            state.tab = tab;
            let serialized = serde_json::to_string(&tree(&state)).unwrap();
            assert!(
                !serialized.contains('\u{2014}'),
                "tab {tab} contained an em-dash"
            );
            if matches!(tab, 0..=3) {
                assert!(
                    serialized.contains("🦀"),
                    "tab {tab} should include Rust emoji indicator"
                );
            }
            if tab == 4 {
                assert!(serialized.contains("L017"));
            }
        }
    }
}
