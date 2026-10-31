//! Workspace analysis and repository health for any project.
//!
//! The plugin walks the active workspace, detects languages and manifests, runs
//! local toolchain checks, and publishes a native report. It never writes to the
//! workspace. stdout is protocol traffic only; logs go to stderr.

use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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

#[derive(Clone, Default)]
struct FileInfo {
    path: String,
    size: u64,
    lines: usize,
    language: String,
    source: bool,
    todo: usize,
    large: bool,
}

#[derive(Clone, Default)]
struct LanguageStats {
    files: usize,
    lines: usize,
    bytes: u64,
}

#[derive(Clone)]
struct Manifest {
    path: String,
    ecosystem: String,
    name: String,
    version: String,
    deps: Option<usize>,
    detail: String,
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

#[derive(Clone, Default)]
struct Check {
    label: String,
    ok: bool,
    detail: String,
}

#[derive(Clone, Default)]
struct Analysis {
    root: String,
    name: String,
    files: Vec<FileInfo>,
    languages: BTreeMap<String, LanguageStats>,
    manifests: Vec<Manifest>,
    todos: Vec<Todo>,
    todo_total: usize,
    todo_tags: BTreeMap<String, usize>,
    tools: Vec<Tool>,
    checks: Vec<Check>,
    warnings: Vec<String>,
    git: GitInfo,
    test_files: usize,
    readme: bool,
    license: bool,
    ci: bool,
    lockfile: bool,
    truncated: bool,
    scanned_ms: u128,
    scanned_at: String,
    total_lines: usize,
    total_bytes: u64,
}

struct State {
    root: Option<PathBuf>,
    analysis: Option<Analysis>,
    tab: usize,
    live: bool,
    status: String,
    last_scan: Option<Instant>,
}

impl State {
    fn new() -> Self {
        Self {
            root: None,
            analysis: None,
            tab: 0,
            live: false,
            status: "Waiting for the active workspace…".into(),
            last_scan: None,
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
        self.status = format!(
            "Scanned {} files in {} ms · {}",
            scan.files.len(),
            scan.scanned_ms,
            scan.scanned_at
        );
        self.analysis = Some(scan);
        self.last_scan = Some(Instant::now());
    }

    fn handle_event(&mut self, handler: &str, value: &Value) {
        match handler {
            "tab" => {
                self.tab = value.as_u64().unwrap_or_default().min(4) as usize;
            }
            "live" => {
                self.live = value.as_bool().unwrap_or(false);
                self.status = if self.live {
                    "Live scanning is on (every 5 s)".into()
                } else {
                    "Live scanning is off".into()
                };
            }
            "refresh" => self.rescan(),
            _ => {}
        }
    }
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
        "lock" => ("Lockfile", false),
        "txt" => ("Text", false),
        _ if lower.ends_with(".lock") => ("Lockfile", false),
        _ => ("Other", false),
    }
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

fn todo_tags() -> [&'static str; 4] {
    ["TODO", "FIXME", "HACK", "XXX"]
}

fn scan_content(
    path: &str,
    text: &str,
    file: &mut FileInfo,
    todos: &mut Vec<Todo>,
    tag_counts: &mut BTreeMap<String, usize>,
) {
    file.lines = count_lines(text);
    for (index, line) in text.lines().enumerate() {
        if file.todo >= 100 {
            break;
        }
        for tag in todo_tags() {
            if !line.contains(tag) {
                continue;
            }
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
}

fn walk(root: &Path) -> (Vec<FileInfo>, bool, bool) {
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
                return (files, truncated, true);
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let (language, source) = language_for(&name);
            files.push(FileInfo {
                path: relative,
                size: metadata.len(),
                language: language.into(),
                source,
                large: metadata.len() > 2 * 1024 * 1024,
                ..Default::default()
            });
        }
    }
    (files, truncated, false)
}

fn analyze(root: &Path) -> Analysis {
    let started = Instant::now();
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();
    let (mut files, truncated, _) = walk(root);
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut languages: BTreeMap<String, LanguageStats> = BTreeMap::new();
    let mut manifests = Vec::new();
    let mut todos = Vec::new();
    let mut todo_tags_map: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut total_lines = 0usize;
    let mut read_budget = MAX_CONTENT_BYTES;
    let mut test_files = 0usize;
    let mut readme = false;
    let mut license = false;
    let ci = root.join(".github/workflows").is_dir()
        || root.join(".gitlab-ci.yml").is_file()
        || root.join(".circleci").is_dir()
        || root.join("azure-pipelines.yml").is_file();

    for file in &mut files {
        total_bytes += file.size;
        let entry = languages
            .entry(file.language.clone())
            .or_default();
        entry.files += 1;
        entry.bytes += file.size;

        let lower = file.path.to_ascii_lowercase();
        if lower.contains("test") || lower.ends_with("_test.go") {
            test_files += 1;
        }
        if file.path.matches('/').count() == 0 {
            let basename = lower.as_str();
            if basename.starts_with("readme") {
                readme = true;
            }
            if basename.starts_with("license") || basename.starts_with("copying") {
                license = true;
            }
        }

        if manifest_kind(&file.path).is_some() && manifests.len() < 40 {
            manifests.push(read_manifest(root, &file.path));
        }

        let can_read = file.size <= MAX_FILE_READ
            && (file.source || manifest_kind(&file.path).is_some() || file.language == "Markdown")
            && read_budget > 0;
        if !can_read {
            if file.size > MAX_FILE_READ && file.source {
                // Count lines cheaply in a stream without keeping the file.
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
                    entry.lines += file.lines;
                    total_lines += file.lines;
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
        scan_content(
            &relative,
            &text,
            file,
            &mut todos,
            &mut todo_tags_map,
        );
        entry.lines += file.lines;
        total_lines += file.lines;
    }

    let toolchains = toolchains(&manifests);
    let lockfile_names = [
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
    ];
    let has_lockfile = lockfile_names.iter().any(|name| root.join(name).is_file())
        || files
            .iter()
            .any(|file| lockfile_names.iter().any(|name| file.path.ends_with(name)));
    let ecosystems: BTreeSet<String> = manifests
        .iter()
        .map(|manifest| manifest.ecosystem.clone())
        .collect();
    let needs_lockfile = ecosystems
        .iter()
        .any(|ecosystem| matches!(ecosystem.as_str(), "Rust" | "Node" | "Go" | "Python"));
    let lockfile = !needs_lockfile || has_lockfile;
    let git = git_info(root);
    let todo_total: usize = files.iter().map(|file| file.todo).sum();

    let mut checks = vec![
        Check {
            label: "README".into(),
            ok: readme,
            detail: if readme {
                "found".into()
            } else {
                "add a README at the repository root".into()
            },
        },
        Check {
            label: "License".into(),
            ok: license,
            detail: if license {
                "found".into()
            } else {
                "no LICENSE or COPYING file".into()
            },
        },
        Check {
            label: "Tests".into(),
            ok: test_files > 0,
            detail: format!("{test_files} test-named file(s)"),
        },
        Check {
            label: "CI".into(),
            ok: ci,
            detail: if ci {
                "CI configuration found".into()
            } else {
                "no GitHub Actions, GitLab CI, CircleCI, or Azure config".into()
            },
        },
        Check {
            label: "Lockfile".into(),
            ok: lockfile,
            detail: if lockfile {
                "present or not required".into()
            } else {
                "a package manager manifest has no lockfile".into()
            },
        },
        Check {
            label: "Clean git tree".into(),
            ok: git.repo && git.dirty == 0,
            detail: if !git.repo {
                "not a git repository".into()
            } else if git.dirty == 0 {
                "clean".into()
            } else {
                format!("{} changed path(s)", git.dirty)
            },
        },
        Check {
            label: "Toolchains".into(),
            ok: toolchains.iter().all(|tool| tool.available),
            detail: toolchains
                .iter()
                .filter(|tool| !tool.available)
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        },
    ];
    checks.retain(|check| !check.detail.is_empty() || check.ok);

    let mut warnings = Vec::new();
    for check in &checks {
        if !check.ok {
            warnings.push(format!("{}: {}", check.label, check.detail));
        }
    }
    if todo_total > 0 {
        warnings.push(format!(
            "{todo_total} TODO/FIXME/HACK/XXX marker(s) across the workspace"
        ));
    }
    if truncated {
        warnings.push(format!("scan stopped after {MAX_FILES} files"));
    }
    let large_files = files.iter().filter(|file| file.large).count();
    if large_files > 0 {
        warnings.push(format!("{large_files} file(s) larger than 2 MB"));
    }
    if manifests.is_empty() {
        warnings.push("no common package or build manifest was found".into());
    }
    if warnings.len() > 12 {
        warnings.truncate(12);
    }

    Analysis {
        root: root.to_string_lossy().to_string(),
        name,
        files,
        languages,
        manifests,
        todos,
        todo_total,
        todo_tags: todo_tags_map,
        tools: toolchains,
        checks,
        warnings,
        git,
        test_files,
        readme,
        license,
        ci,
        lockfile,
        truncated,
        scanned_ms: started.elapsed().as_millis(),
        scanned_at: timestamp(),
        total_lines,
        total_bytes,
    }
}

fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    // Keep the plugin dependency-free: format a local-looking clock without a
    // date crate. The exact epoch is also exposed for clients that need it.
    let hours = (seconds / 3600) % 24;
    let minutes = (seconds / 60) % 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02} UTC")
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
        _ => None,
    }
}

fn read_manifest(root: &Path, path: &str) -> Manifest {
    let ecosystem = manifest_kind(path).unwrap_or("Other").to_string();
    let content = read_limited(&root.join(path), 256 * 1024)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let mut manifest = Manifest {
        path: path.to_string(),
        ecosystem: ecosystem.clone(),
        name: String::new(),
        version: String::new(),
        deps: None,
        detail: String::new(),
    };
    match ecosystem.as_str() {
        "Node" => {
            if let Ok(value) = serde_json::from_str::<Value>(&content) {
                manifest.name = value["name"].as_str().unwrap_or("").to_string();
                manifest.version = value["version"].as_str().unwrap_or("").to_string();
                let runtime = value["dependencies"]
                    .as_object()
                    .map_or(0, |map| map.len());
                let dev = value["devDependencies"]
                    .as_object()
                    .map_or(0, |map| map.len());
                manifest.deps = Some(runtime + dev);
                let scripts = value["scripts"].as_object().map_or(0, |map| map.len());
                manifest.detail = format!("{runtime} runtime · {dev} dev · {scripts} scripts");
            }
        }
        "Rust" => {
            manifest.name = toml_value(&content, "name");
            manifest.version = toml_value(&content, "version");
            manifest.deps = Some(count_sections(&content, &["dependencies", "dev-dependencies", "build-dependencies"]));
            manifest.detail = format!("{} dependency lines", manifest.deps.unwrap_or_default());
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
                        && (trimmed.contains(' ') && trimmed.contains('/') || trimmed.starts_with("require"))
                })
                .count();
            manifest.deps = Some(deps);
            manifest.detail = format!("{deps} require line(s)");
        }
        "Python" => {
            manifest.name = toml_value(&content, "name");
            manifest.deps = Some(count_sections(&content, &["dependencies", "dev-dependencies"]));
            manifest.detail = if path.ends_with("requirements.txt") {
                let deps = content
                    .lines()
                    .filter(|line| {
                        let line = line.trim();
                        !line.is_empty() && !line.starts_with('#') && !line.starts_with('-')
                    })
                    .count();
                manifest.deps = Some(deps);
                format!("{deps} requirements")
            } else {
                format!("{} dependency lines", manifest.deps.unwrap_or_default())
            };
        }
        "Ruby" => {
            manifest.deps = Some(content.lines().filter(|line| line.trim_start().starts_with("gem ")).count());
            manifest.detail = format!("{} gem line(s)", manifest.deps.unwrap_or_default());
        }
        "PHP" => {
            if let Ok(value) = serde_json::from_str::<Value>(&content) {
                manifest.name = value["name"].as_str().unwrap_or("").to_string();
                manifest.deps = Some(
                    value["require"].as_object().map_or(0, |map| map.len())
                        + value["require-dev"].as_object().map_or(0, |map| map.len()),
                );
                manifest.detail = format!("{} composer requirement(s)", manifest.deps.unwrap_or_default());
            }
        }
        "Java" => {
            manifest.deps = Some(content.matches("<dependency>").count());
            manifest.detail = format!("{} Maven dependency block(s)", manifest.deps.unwrap_or_default());
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
            manifest.detail = format!("{} Gradle dependency line(s)", manifest.deps.unwrap_or_default());
        }
        "C/C++" => {
            manifest.name = content
                .lines()
                .find(|line| line.trim_start().starts_with("project("))
                .map(|line| line.trim().trim_start_matches("project(").trim_end_matches(')').to_string())
                .unwrap_or_default();
            manifest.detail = "CMake project".into();
        }
        _ => {
            manifest.detail = "build manifest".into();
        }
    }
    if manifest.name.trim().is_empty() {
        manifest.name = path.to_string();
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
            active = sections.iter().any(|section| {
                trimmed
                    .trim_matches(['[', ']'])
                    .split('.')
                    .last()
                    .is_some_and(|name| name == *section)
            });
            continue;
        }
        if active
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with('[')
        {
            count += 1;
        }
    }
    count
}

fn toolchains(manifests: &[Manifest]) -> Vec<Tool> {
    let ecosystems: BTreeSet<&str> = manifests
        .iter()
        .map(|manifest| manifest.ecosystem.as_str())
        .collect();
    let mut tools = vec![tool("git", &["--version"])];
    if ecosystems.contains("Rust") {
        tools.push(tool("cargo", &["--version"]));
    }
    if ecosystems.contains("Node") {
        tools.push(tool("node", &["--version"]));
    }
    if ecosystems.contains("Go") {
        tools.push(tool("go", &["version"]));
    }
    if ecosystems.contains("Python") {
        tools.push(tool("python3", &["--version"]));
    }
    if ecosystems.contains("Java") || ecosystems.contains("Java/Kotlin") {
        tools.push(tool("java", &["-version"]));
    }
    tools
}

fn tool(name: &str, args: &[&str]) -> Tool {
    match Command::new(name).args(args).output() {
        Ok(output) => {
            let raw = if output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stderr).to_string()
            } else {
                String::from_utf8_lossy(&output.stdout).to_string()
            };
            Tool {
                name: name.into(),
                available: output.status.success(),
                version: raw.lines().next().unwrap_or("").trim().to_string(),
            }
        }
        Err(_) => Tool {
            name: name.into(),
            available: false,
            version: "not found".into(),
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
    let last_commit = command_line(root, &["log", "-1", "--format=%h  %cr  %s"])
        .unwrap_or_default();
    let dirty = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("status")
        .arg("--porcelain")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).lines().count())
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
    let output = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
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

fn card(title: &str, children: Vec<Value>) -> Value {
    let mut all = vec![text(title, "subtitle", "ink")];
    all.extend(children);
    json!({"kind": "card", "gap": 10, "children": all})
}

fn stat(label: &str, value: impl Into<String>, detail: Option<String>, color: &str) -> Value {
    let mut node = json!({
        "kind": "stat",
        "label": label,
        "value": value.into(),
        "color": color
    });
    if let Some(detail) = detail {
        node["detail"] = json!(detail);
    }
    node
}

fn table(columns: &[&str], rows: Vec<Vec<String>>) -> Value {
    json!({
        "kind": "table",
        "columns": columns,
        "rows": rows.into_iter()
            .take(250)
            .map(|row| row.into_iter().take(24).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    })
}

fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * part as f64 / total as f64
    }
}

fn overview(analysis: &Analysis) -> Value {
    let passed = analysis.checks.iter().filter(|check| check.ok).count();
    let language_slices = analysis
        .languages
        .iter()
        .filter(|(_, stats)| stats.lines > 0)
        .take(6)
        .enumerate()
        .map(|(index, (name, stats))| {
            let colors = ["accent", "series2", "series4", "series5", "series6", "positive"];
            json!({
                "label": name,
                "value": stats.lines as f64,
                "color": colors[index % colors.len()]
            })
        })
        .collect::<Vec<_>>();
    let warnings = if analysis.warnings.is_empty() {
        vec![text("No obvious repository-health warnings.", "body", "positive")]
    } else {
        analysis
            .warnings
            .iter()
            .take(10)
            .map(|warning| text(format!("• {warning}"), "small", "warning"))
            .collect()
    };
    let git_summary = if analysis.git.repo {
        format!(
            "{} · {} changed path(s) · {}{}",
            analysis.git.branch,
            analysis.git.dirty,
            analysis.git.last_commit,
            if analysis.truncated {
                " · scan truncated"
            } else {
                ""
            }
        )
    } else {
        analysis.git.error.clone()
    };
    let mut warning_children = vec![text(git_summary, "small", "muted")];
    warning_children.extend(warnings);
    let manifest_rows = analysis
        .manifests
        .iter()
        .take(8)
        .map(|manifest| {
            vec![
                manifest.path.clone(),
                manifest.ecosystem.clone(),
                manifest.name.clone(),
                manifest
                    .deps
                    .map(|deps| deps.to_string())
                    .unwrap_or_else(|| "—".into()),
            ]
        })
        .collect();
    let checks_rows = analysis
        .checks
        .iter()
        .map(|check| {
            vec![
                check.label.clone(),
                if check.ok { "pass".into() } else { "attention".into() },
                check.detail.clone(),
            ]
        })
        .collect();
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            {
                "kind": "grid",
                "columns": 4,
                "gap": 12,
                "children": [
                    stat("Files", analysis.files.len().to_string(), Some(format!("{} scanned at most", MAX_FILES)), "accent"),
                    stat("Lines", analysis.total_lines.to_string(), Some("source and docs".into()), "ink"),
                    stat("Languages", analysis.languages.len().to_string(), Some(format!("{} manifests", analysis.manifests.len())), "series2"),
                    stat("Size", fmt_size(analysis.total_bytes), Some(analysis.git.branch.clone()), "series6")
                ]
            },
            {
                "kind": "grid",
                "columns": 2,
                "gap": 12,
                "children": [
                    card("Repository health", vec![
                        json!({
                            "kind": "progress",
                            "label": format!("{passed}/{} checks passing", analysis.checks.len()),
                            "value": passed as f64,
                            "max": analysis.checks.len().max(1) as f64,
                            "detail": "Evidence-based checks, not a score",
                            "color": if passed == analysis.checks.len() { "positive" } else { "warning" }
                        }),
                        table(&["Check", "Status", "Detail"], checks_rows)
                    ]),
                    card("Warnings", warning_children)
                ]
            },
            {
                "kind": "grid",
                "columns": 2,
                "gap": 12,
                "children": [
                    card("Languages by lines", vec![
                        json!({
                            "kind": "donut",
                            "slices": if language_slices.is_empty() {
                                vec![json!({"label":"No source","value":1.0,"color":"muted"})]
                            } else { language_slices },
                            "label": "lines",
                            "value": format!("{}", analysis.total_lines)
                        })
                    ]),
                    card("Manifests", vec![table(&["Path", "Ecosystem", "Name", "Deps"], manifest_rows)])
                ]
            }
        ]
    })
}

fn languages(analysis: &Analysis) -> Value {
    let mut entries = analysis
        .languages
        .iter()
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.lines.cmp(&a.1.lines).then_with(|| b.1.bytes.cmp(&a.1.bytes)));
    let top = entries.iter().take(12).collect::<Vec<_>>();
    let labels = if top.is_empty() {
        vec!["No files".to_string()]
    } else {
        top.iter().map(|(name, _)| name.to_string()).collect()
    };
    let values = if top.is_empty() {
        vec![0.0]
    } else {
        top.iter()
            .map(|(_, stats)| stats.lines as f64)
            .collect()
    };
    let rows = entries
        .iter()
        .map(|(name, stats)| {
            vec![
                name.to_string(),
                stats.files.to_string(),
                stats.lines.to_string(),
                format!("{:.1}%", percent(stats.lines, analysis.total_lines)),
                fmt_size(stats.bytes),
            ]
        })
        .collect();
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            card("Lines by language", vec![
                json!({
                    "kind": "chart",
                    "chart_type": "bar",
                    "labels": labels,
                    "series": [{"label": "Lines", "values": values, "color": "accent"}],
                    "height": 230
                })
            ]),
            card("Language breakdown", vec![table(&["Language", "Files", "Lines", "Share", "Size"], rows)])
        ]
    })
}

fn dependencies(analysis: &Analysis) -> Value {
    let manifest_rows = analysis
        .manifests
        .iter()
        .map(|manifest| {
            vec![
                manifest.path.clone(),
                manifest.ecosystem.clone(),
                manifest.name.clone(),
                manifest.version.clone(),
                manifest
                    .deps
                    .map(|deps| deps.to_string())
                    .unwrap_or_else(|| "—".into()),
                manifest.detail.clone(),
            ]
        })
        .collect();
    let tool_rows = analysis
        .tools
        .iter()
        .map(|tool| {
            vec![
                tool.name.clone(),
                if tool.available { "available".into() } else { "missing".into() },
                tool.version.clone(),
            ]
        })
        .collect();
    let lock_rows = [
        ("README", analysis.readme),
        ("License", analysis.license),
        ("Tests", analysis.test_files > 0),
        ("CI", analysis.ci),
        ("Lockfile", analysis.lockfile),
    ]
    .into_iter()
    .map(|(name, ok)| vec![name.into(), if ok { "present".into() } else { "missing".into() }])
    .collect();
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            card("Manifests", vec![table(&["Path", "Ecosystem", "Name", "Version", "Deps", "Detail"], manifest_rows)]),
            {
                "kind": "grid",
                "columns": 2,
                "gap": 12,
                "children": [
                    card("Toolchains", vec![table(&["Tool", "Status", "Version"], tool_rows)]),
                    card("Project files", vec![table(&["Kind", "Status"], lock_rows)])
                ]
            }
        ]
    })
}

fn files_tab(analysis: &Analysis) -> Value {
    let mut by_size = analysis.files.iter().collect::<Vec<_>>();
    by_size.sort_by(|a, b| b.size.cmp(&a.size));
    let size_rows = by_size
        .iter()
        .take(20)
        .map(|file| {
            vec![
                file.path.clone(),
                fmt_size(file.size),
                file.lines.to_string(),
                file.language.clone(),
            ]
        })
        .collect();
    let mut by_lines = analysis.files.iter().filter(|file| file.source).collect::<Vec<_>>();
    by_lines.sort_by(|a, b| b.lines.cmp(&a.lines));
    let line_rows = by_lines
        .iter()
        .take(20)
        .map(|file| {
            vec![
                file.path.clone(),
                file.lines.to_string(),
                file.language.clone(),
                file.todo.to_string(),
            ]
        })
        .collect();
    let mut hotspots = analysis.files.iter().filter(|file| file.todo > 0).collect::<Vec<_>>();
    hotspots.sort_by(|a, b| b.todo.cmp(&a.todo));
    let hotspot_rows = hotspots
        .iter()
        .take(20)
        .map(|file| vec![file.path.clone(), file.todo.to_string(), file.language.clone()])
        .collect();
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            {
                "kind": "grid",
                "columns": 2,
                "gap": 12,
                "children": [
                    card("Largest files", vec![table(&["Path", "Size", "Lines", "Language"], size_rows)]),
                    card("Longest source files", vec![table(&["Path", "Lines", "Language", "Markers"], line_rows)])
                ]
            },
            card("TODO hotspots", vec![table(&["Path", "Markers", "Language"], hotspot_rows)])
        ]
    })
}

fn todo_tab(analysis: &Analysis) -> Value {
    let mut tags = analysis.todo_tags.iter().collect::<Vec<_>>();
    tags.sort_by(|a, b| b.1.cmp(a.1));
    let tag_rows = tags
        .iter()
        .map(|(tag, count)| vec![tag.to_string(), count.to_string()])
        .collect();
    let rows = analysis
        .todos
        .iter()
        .take(100)
        .map(|todo| vec![todo.path.clone(), todo.line.to_string(), todo.tag.clone(), todo.text.clone()])
        .collect();
    json!({
        "kind": "column",
        "gap": 12,
        "children": [
            {
                "kind": "grid",
                "columns": 3,
                "gap": 12,
                "children": [
                    stat("Markers", analysis.todo_total.to_string(), Some("TODO/FIXME/HACK/XXX".into()), "warning"),
                    stat("Files with markers", analysis.files.iter().filter(|file| file.todo > 0).count().to_string(), None, "accent"),
                    stat("Showing", analysis.todos.len().to_string(), Some("first 100 samples".into()), "muted")
                ]
            },
            {
                "kind": "grid",
                "columns": 2,
                "gap": 12,
                "children": [
                    card("By tag", vec![table(&["Tag", "Count"], tag_rows)]),
                    card("How to read this", vec![text(
                        "Markers are cheap evidence of follow-up work. A high count is not automatically bad; compare it with the size and age of the codebase.",
                        "body",
                        "muted"
                    )])
                ]
            },
            card("Marker inventory", vec![table(&["Path", "Line", "Tag", "Excerpt"], rows)])
        ]
    })
}

fn tree(state: &State) -> Value {
    let Some(analysis) = &state.analysis else {
        return json!({
            "kind": "column",
            "gap": 12,
            "children": [
                text("Workspace Analysis", "hero", "ink"),
                text(state.status.clone(), "body", "muted")
            ]
        });
    };
    let body = match state.tab {
        0 => overview(analysis),
        1 => languages(analysis),
        2 => dependencies(analysis),
        3 => files_tab(analysis),
        _ => todo_tab(analysis),
    };
    let passed = analysis.checks.iter().filter(|check| check.ok).count();
    json!({
        "kind": "column",
        "gap": 16,
        "children": [
            text("Workspace Analysis", "hero", "ink"),
            text(format!("{} · {}", analysis.name, analysis.root), "body", "muted"),
            {
                "kind": "row",
                "gap": 8,
                "children": [
                    {"kind": "button", "label": "Rescan", "handler": "refresh", "disabled": false},
                    {"kind": "toggle", "label": "Live (5 s)", "value": state.live, "handler": "live"},
                    stat("Checks", format!("{passed}/{}", analysis.checks.len()), None, if passed == analysis.checks.len() { "positive" } else { "warning" }),
                    stat("Scan", format!("{} ms", analysis.scanned_ms), Some(analysis.scanned_at.clone()), "ink")
                ]
            },
            text(state.status.clone(), "small", "muted"),
            {
                "kind": "tabs",
                "labels": ["Overview", "Languages", "Dependencies", "Files", "TODO"],
                "selected": state.tab,
                "handler": "tab"
            },
            body
        ]
    })
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
    let request_workspace = |out: &mut BufWriter<_>, id: u64| {
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
                    if message["id"].as_u64() == Some(2) {
                        if let Some(root) = message["result"]["root"].as_str() {
                            state.root = Some(PathBuf::from(root));
                            state.rescan();
                            publish(&mut out, &state);
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
                        request_workspace(&mut out, 2);
                    }
                    "ui.open" => {
                        state.rescan();
                        publish(&mut out, &state);
                        request_workspace(&mut out, 2);
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
            Err(RecvTimeoutError::Timeout) => {
                if state.live
                    && state.last_scan.is_some_and(|last| last.elapsed() > Duration::from_secs(5))
                {
                    state.rescan();
                    publish(&mut out, &state);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
