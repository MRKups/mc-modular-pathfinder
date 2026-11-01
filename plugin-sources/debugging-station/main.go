//! A read-only process and port inspector.
//!
//! It samples processes and sockets with ordinary OS tools, lets the user pick
//! a project target, and renders the inspector through Monocode's native
//! component schema. stdout is protocol traffic only; diagnostics go to stderr.

package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io/fs"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

const surface = "debugging-station"

type process struct {
	PID     int
	PPID    int
	CPU     float64
	Memory  float64
	RSS     int64
	State   string
	Command string
	Args    string
}

type connection struct {
	Protocol string
	Local    string
	Remote   string
	State    string
	Port     string
	Process  string
	PID      int
}

type disk struct {
	Mount   string
	Size    int64
	Used    int64
	Free    int64
	Percent string
}

type buildCommand struct {
	Label   string
	Command string
	Source  string
}

type buildTarget struct {
	Name    string
	Command string
	Source  string
}

type buildSystem struct {
	Directory string
	Manifest  string
	Name      string
	System    string
	Manager   string
	Detail    string
	Commands  []buildCommand
	Targets   []buildTarget
	Tools     []string
}

type toolInfo struct {
	Name      string
	Available bool
	Version   string
	NeededBy  string
}

type snapshot struct {
	Processes   []process
	Connections []connection
	Listeners   []connection
	States      map[string]int
	Load1       float64
	Load5       float64
	Load15      float64
	CPUs        int
	MemoryTotal int64
	MemoryUsed  int64
	MemoryFree  int64
	SwapTotal   int64
	SwapUsed    int64
	Disks       []disk
	Hostname    string
	OS          string
	Arch        string
	Uptime      string
	Project     string
	Errors      []string
	SampledAt   string
	SampledMS   int64
}

type State struct {
	Project       string
	Snapshot      *snapshot
	Tab           int
	TargetPID     int
	Sort          int
	Filter        string
	RequestID     int
	Build         []buildSystem
	BuildTools    []toolInfo
	BuildWarnings []string
	BuildRoot     string
	BuildScanned  string
	BuildMS       int64
}

func newState() *State {
	return &State{
		Tab:       0,
		TargetPID: 0,
		RequestID: 2,
	}
}

func send(message map[string]any) {
	line, err := json.Marshal(message)
	if err != nil {
		return
	}
	if _, err := fmt.Fprintf(os.Stdout, "%s\n", line); err != nil {
		os.Exit(0)
	}
	_ = os.Stdout.Sync()
}

func respond(id any, result any) {
	send(map[string]any{"jsonrpc": "2.0", "id": id, "result": result})
}

func notify(method string, params map[string]any) {
	send(map[string]any{"jsonrpc": "2.0", "method": method, "params": params})
}

func text(value string, size, color string) map[string]any {
	return map[string]any{
		"kind":  "text",
		"text":  clean(value),
		"size":  size,
		"color": color,
	}
}

func clean(value string) string {
	return strings.ReplaceAll(value, "\x00", "")
}

func section(title string, children []any) map[string]any {
	all := []any{text(title, "subtitle", "ink")}
	all = append(all, children...)
	return map[string]any{"kind": "column", "gap": 8, "children": all}
}

func table(columns []string, rows [][]string, weights ...float64) map[string]any {
	cleanRows := make([][]string, 0, min(len(rows), 250))
	for _, row := range rows {
		if len(cleanRows) >= 250 {
			break
		}
		cleanRow := make([]string, 0, min(len(row), 24))
		for _, cell := range row {
			if len(cleanRow) >= 24 {
				break
			}
			cleanRow = append(cleanRow, clean(cell))
		}
		cleanRows = append(cleanRows, cleanRow)
	}
	node := map[string]any{"kind": "table", "columns": columns, "rows": cleanRows}
	if len(weights) == len(columns) {
		node["weights"] = weights
	}
	return node
}

func button(label, handler string, disabled bool) map[string]any {
	return map[string]any{"kind": "button", "label": label, "handler": handler, "disabled": disabled}
}

func toggle(label string, value bool, handler string) map[string]any {
	return map[string]any{"kind": "toggle", "label": label, "value": value, "handler": handler}
}

func run(name string, args ...string) (string, error) {
	return runTimeout(5*time.Second, name, args...)
}

func runTimeout(timeout time.Duration, name string, args ...string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	command := exec.CommandContext(ctx, name, args...)
	output, err := command.Output()
	if err != nil {
		return string(output), err
	}
	return string(output), nil
}

func runFirst(candidates [][]string) (string, string) {
	var lastErr error
	for _, candidate := range candidates {
		if len(candidate) == 0 {
			continue
		}
		output, err := run(candidate[0], candidate[1:]...)
		if err == nil && strings.TrimSpace(output) != "" {
			return output, ""
		}
		if err != nil {
			lastErr = err
		}
	}
	if lastErr == nil {
		lastErr = fmt.Errorf("no command available")
	}
	return "", lastErr.Error()
}

func parseFloat(value string) float64 {
	number, err := strconv.ParseFloat(strings.TrimSpace(value), 64)
	if err != nil || math.IsNaN(number) || math.IsInf(number, 0) {
		return 0
	}
	return number
}

func parseInt(value string) int64 {
	number, err := strconv.ParseInt(strings.TrimSpace(value), 10, 64)
	if err != nil {
		return 0
	}
	return number
}

func parseProcesses() ([]process, string) {
	output, err := run("ps", "-axo", "pid=,ppid=,pcpu=,pmem=,rss=,stat=,comm=,args=")
	if err != nil {
		return nil, fmt.Sprintf("ps: %v", err)
	}
	processes := make([]process, 0, 256)
	for _, line := range strings.Split(output, "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) < 7 {
			continue
		}
		pid, _ := strconv.Atoi(fields[0])
		ppid, _ := strconv.Atoi(fields[1])
		command := fields[6]
		args := command
		if len(fields) > 7 {
			args = strings.Join(fields[7:], " ")
		}
		processes = append(processes, process{
			PID:     pid,
			PPID:    ppid,
			CPU:     parseFloat(fields[2]),
			Memory:  parseFloat(fields[3]),
			RSS:     parseInt(fields[4]) * 1024,
			State:   fields[5],
			Command: command,
			Args:    args,
		})
	}
	return processes, ""
}

func loadAverage() (float64, float64, float64, string) {
	if runtime.GOOS == "linux" {
		data, err := os.ReadFile("/proc/loadavg")
		if err == nil {
			fields := strings.Fields(string(data))
			if len(fields) >= 3 {
				return parseFloat(fields[0]), parseFloat(fields[1]), parseFloat(fields[2]), ""
			}
		}
		return 0, 0, 0, "cannot read /proc/loadavg"
	}
	output, err := run("sysctl", "-n", "vm.loadavg")
	if err != nil {
		return 0, 0, 0, fmt.Sprintf("load average: %v", err)
	}
	fields := strings.Fields(strings.Trim(output, "{ }\n"))
	if len(fields) < 3 {
		return 0, 0, 0, "cannot parse load average"
	}
	return parseFloat(fields[0]), parseFloat(fields[1]), parseFloat(fields[2]), ""
}

func memory() (int64, int64, int64, int64, int64, string) {
	if runtime.GOOS == "linux" {
		data, err := os.ReadFile("/proc/meminfo")
		if err != nil {
			return 0, 0, 0, 0, 0, fmt.Sprintf("memory: %v", err)
		}
		values := map[string]int64{}
		for _, line := range strings.Split(string(data), "\n") {
			fields := strings.Fields(line)
			if len(fields) < 2 {
				continue
			}
			values[strings.TrimSuffix(fields[0], ":")] = parseInt(fields[1]) * 1024
		}
		total := values["MemTotal"]
		available := values["MemAvailable"]
		return total, total - available, available, values["SwapTotal"], values["SwapTotal"] - values["SwapFree"], ""
	}
	output, err := run("sysctl", "-n", "hw.memsize")
	if err != nil {
		return 0, 0, 0, 0, 0, fmt.Sprintf("memory: %v", err)
	}
	total := parseInt(strings.TrimSpace(output))
	pageSize := int64(4096)
	if value, err := run("sysctl", "-n", "hw.pagesize"); err == nil {
		pageSize = parseInt(strings.TrimSpace(value))
		if pageSize <= 0 {
			pageSize = 4096
		}
	}
	vm, err := run("vm_stat")
	if err != nil {
		return total, 0, total, 0, 0, fmt.Sprintf("vm_stat: %v", err)
	}
	pages := map[string]int64{}
	for _, line := range strings.Split(vm, "\n") {
		parts := strings.SplitN(line, ":", 2)
		if len(parts) != 2 {
			continue
		}
		key := strings.TrimSpace(parts[0])
		value := strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(parts[1]), "."))
		pages[key] = parseInt(value)
	}
	free := (pages["Pages free"] + pages["Pages inactive"] + pages["Pages speculative"]) * pageSize
	used := total - free
	if used < 0 {
		used = 0
	}
	return total, used, free, 0, 0, ""
}

func diskInfo() ([]disk, string) {
	output, err := run("df", "-k")
	if err != nil {
		return nil, fmt.Sprintf("df: %v", err)
	}
	mountIndex := 5
	if runtime.GOOS == "darwin" {
		// macOS includes iused/ifree/%iused before the mount point.
		mountIndex = 8
	}
	disks := make([]disk, 0, 8)
	for _, line := range strings.Split(output, "\n")[1:] {
		fields := strings.Fields(line)
		if len(fields) <= mountIndex {
			continue
		}
		size := parseInt(fields[1])
		if size <= 0 {
			continue
		}
		filesystem := fields[0]
		if strings.HasPrefix(filesystem, "devfs") || strings.HasPrefix(filesystem, "map ") {
			continue
		}
		disks = append(disks, disk{
			Mount:   strings.Join(fields[mountIndex:], " "),
			Size:    size * 1024,
			Used:    parseInt(fields[2]) * 1024,
			Free:    parseInt(fields[3]) * 1024,
			Percent: fields[4],
		})
		if len(disks) >= 8 {
			break
		}
	}
	return disks, ""
}

func parsePort(address string) string {
	address = strings.TrimSpace(address)
	if address == "" || address == "*.*" || address == "*:*" {
		return ""
	}
	if index := strings.LastIndex(address, ":"); index >= 0 {
		return address[index+1:]
	}
	if index := strings.LastIndex(address, "."); index >= 0 {
		return address[index+1:]
	}
	return ""
}

func network() ([]connection, []connection, map[string]int, string) {
	output, err := run("lsof", "-nP", "-i")
	if strings.TrimSpace(output) != "" {
		return parseLsof(output)
	}
	if runtime.GOOS == "darwin" {
		// Some minimal macOS images ship a netstat that omits Internet sockets.
		// lsof is the richer source there; if both fail, surface the error.
		if err != nil {
			return nil, nil, map[string]int{}, fmt.Sprintf("lsof: %v", err)
		}
	}
	return networkNetstat()
}

func parseLsof(output string) ([]connection, []connection, map[string]int, string) {
	connections := make([]connection, 0, 256)
	listeners := make([]connection, 0, 64)
	states := map[string]int{}
	for _, line := range strings.Split(output, "\n")[1:] {
		fields := strings.Fields(line)
		if len(fields) < 9 || (fields[4] != "IPv4" && fields[4] != "IPv6") {
			continue
		}
		protocol := strings.ToLower(fields[7])
		if protocol != "tcp" && protocol != "udp" {
			continue
		}
		name := strings.Join(fields[8:], " ")
		state := ""
		if start := strings.LastIndex(name, "("); start >= 0 {
			state = strings.TrimSuffix(name[start+1:], ")")
			name = strings.TrimSpace(name[:start])
		}
		local := name
		remote := "*"
		if arrow := strings.Index(name, "->"); arrow >= 0 {
			local = name[:arrow]
			remote = name[arrow+2:]
		}
		pid, _ := strconv.Atoi(fields[1])
		item := connection{
			Protocol: protocol,
			Local:    local,
			Remote:   remote,
			State:    state,
			Port:     parsePort(local),
			Process:  fields[0],
			PID:      pid,
		}
		if state != "" && state != "LISTEN" {
			states[state]++
		} else if state == "LISTEN" {
			states["LISTEN"]++
		} else {
			states["OPEN"]++
		}
		if state == "LISTEN" || (protocol == "udp" && remote == "*") {
			listeners = append(listeners, item)
		} else {
			connections = append(connections, item)
		}
	}
	return connections, listeners, states, ""
}

func networkNetstat() ([]connection, []connection, map[string]int, string) {
	output, err := run("netstat", "-an")
	if err != nil {
		return nil, nil, map[string]int{}, fmt.Sprintf("netstat: %v", err)
	}
	connections := make([]connection, 0, 256)
	listeners := make([]connection, 0, 64)
	states := map[string]int{}
	for _, line := range strings.Split(output, "\n") {
		fields := strings.Fields(line)
		if len(fields) < 5 {
			continue
		}
		protocol := strings.ToLower(fields[0])
		if !strings.HasPrefix(protocol, "tcp") && !strings.HasPrefix(protocol, "udp") {
			continue
		}
		local := fields[3]
		remote := fields[4]
		state := ""
		if len(fields) > 5 && strings.HasPrefix(protocol, "tcp") {
			state = fields[5]
		}
		item := connection{
			Protocol: protocol,
			Local:    local,
			Remote:   remote,
			State:    state,
			Port:     parsePort(local),
		}
		isListener := state == "LISTEN"
		if strings.HasPrefix(protocol, "udp") && (remote == "*.*" || remote == "*:*" || remote == "0.0.0.0:*") {
			isListener = true
		}
		if state != "" {
			states[state]++
		} else {
			states["OPEN"]++
		}
		if isListener {
			listeners = append(listeners, item)
		} else {
			connections = append(connections, item)
		}
	}
	return connections, listeners, states, ""
}

func hostname() string {
	name, err := os.Hostname()
	if err != nil {
		return "unknown"
	}
	return name
}

func uptime() string {
	if runtime.GOOS == "linux" {
		data, err := os.ReadFile("/proc/uptime")
		if err == nil {
			fields := strings.Fields(string(data))
			if len(fields) > 0 {
				return humanDuration(time.Duration(parseFloat(fields[0]) * float64(time.Second)))
			}
		}
		return "unknown"
	}
	output, err := run("sysctl", "-n", "kern.boottime")
	if err != nil {
		return "unknown"
	}
	index := strings.Index(output, "sec =")
	if index < 0 {
		return "unknown"
	}
	rest := strings.TrimSpace(output[index+5:])
	rest = strings.TrimLeft(rest, " =")
	end := strings.IndexAny(rest, ",}")
	if end >= 0 {
		rest = rest[:end]
	}
	seconds := parseInt(rest)
	if seconds <= 0 {
		return "unknown"
	}
	return humanDuration(time.Since(time.Unix(seconds, 0)))
}

func humanDuration(duration time.Duration) string {
	if duration <= 0 {
		return "unknown"
	}
	days := int(duration.Hours()) / 24
	hours := int(duration.Hours()) % 24
	minutes := int(duration.Minutes()) % 60
	if days > 0 {
		return fmt.Sprintf("%dd %dh %dm", days, hours, minutes)
	}
	return fmt.Sprintf("%dh %dm", hours, minutes)
}

func humanBytes(value int64) string {
	if value <= 0 {
		return "0 B"
	}
	units := []string{"B", "KB", "MB", "GB", "TB"}
	number := float64(value)
	index := 0
	for number >= 1024 && index < len(units)-1 {
		number /= 1024
		index++
	}
	if index == 0 {
		return fmt.Sprintf("%.0f %s", number, units[index])
	}
	return fmt.Sprintf("%.1f %s", number, units[index])
}

const maxBuildSystems = 24

var buildSkipDirs = map[string]bool{
	".git": true, ".hg": true, ".svn": true, ".venv": true, "venv": true,
	"node_modules": true, "target": true, "dist": true, "build": true,
	"vendor": true, "Pods": true, "DerivedData": true, "__pycache__": true,
	".next": true, ".turbo": true, "coverage": true, "plugins": true, "output": true,
}

func relativeDir(root, dir string) string {
	rel, err := filepath.Rel(root, dir)
	if err != nil || rel == "." || rel == "" {
		return "."
	}
	return filepath.ToSlash(rel)
}

func exists(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info != nil
}

func readText(path string, limit int64) string {
	file, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer file.Close()
	data := make([]byte, limit)
	count, _ := file.Read(data)
	return string(data[:count])
}

func stringField(value map[string]any, key string) string {
	if value == nil {
		return ""
	}
	text, _ := value[key].(string)
	return strings.TrimSpace(text)
}

func objectField(value map[string]any, key string) map[string]any {
	if value == nil {
		return nil
	}
	object, _ := value[key].(map[string]any)
	return object
}

func compact(value string, limit int) string {
	value = strings.Join(strings.Fields(value), " ")
	if len(value) > limit {
		return value[:limit-1] + "…"
	}
	return value
}

func indentWidth(line string) int {
	width := 0
	for _, char := range line {
		if char == ' ' {
			width++
		} else if char == '\t' {
			width += 2
		} else {
			break
		}
	}
	return width
}

func tomlValue(content, section, key string) string {
	active := section == ""
	for _, line := range strings.Split(content, "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
			active = section == "" || strings.TrimSpace(strings.Trim(trimmed, "[]")) == section
			continue
		}
		if !active || trimmed == "" || strings.HasPrefix(trimmed, "#") || !strings.HasPrefix(trimmed, key) {
			continue
		}
		rest := strings.TrimSpace(strings.TrimPrefix(trimmed, key))
		if !strings.HasPrefix(rest, "=") {
			continue
		}
		value := strings.TrimSpace(strings.TrimPrefix(rest, "="))
		if strings.HasPrefix(value, "\"") {
			if end := strings.Index(value[1:], "\""); end >= 0 {
				return value[1 : end+1]
			}
		}
		return strings.Fields(value)[0]
	}
	return ""
}

func tomlSectionKeys(content, section string) []string {
	active := false
	keys := make([]string, 0, 12)
	for _, line := range strings.Split(content, "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
			active = strings.TrimSpace(strings.Trim(trimmed, "[]")) == section
			continue
		}
		if !active || trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		index := strings.Index(trimmed, "=")
		if index <= 0 {
			continue
		}
		key := strings.TrimSpace(strings.Trim(trimmed[:index], "\"'"))
		if key != "" {
			keys = append(keys, key)
		}
		if len(keys) >= 20 {
			break
		}
	}
	return keys
}

func uniqueStrings(values []string) []string {
	seen := make(map[string]bool, len(values))
	result := make([]string, 0, len(values))
	for _, value := range values {
		value = strings.TrimSpace(value)
		if value == "" || seen[value] {
			continue
		}
		seen[value] = true
		result = append(result, value)
	}
	return result
}

func nodeManager(dir string, pkg map[string]any) string {
	if declared := stringField(pkg, "packageManager"); declared != "" {
		for _, manager := range []string{"npm", "pnpm", "yarn", "bun"} {
			if strings.HasPrefix(declared, manager+"@") {
				return manager
			}
		}
	}
	for _, candidate := range []struct {
		file    string
		manager string
	}{
		{"pnpm-lock.yaml", "pnpm"},
		{"yarn.lock", "yarn"},
		{"bun.lock", "bun"},
		{"bun.lockb", "bun"},
		{"package-lock.json", "npm"},
	} {
		if exists(filepath.Join(dir, candidate.file)) {
			return candidate.manager
		}
	}
	return "npm"
}

func managerRun(manager, script string) string {
	switch manager {
	case "yarn":
		return "yarn run " + script
	case "pnpm":
		return "pnpm run " + script
	case "bun":
		return "bun run " + script
	default:
		return "npm run " + script
	}
}

func nodeBuildSystem(dir, rel string) buildSystem {
	content := readText(filepath.Join(dir, "package.json"), 512*1024)
	var pkg map[string]any
	_ = json.Unmarshal([]byte(content), &pkg)
	name := stringField(pkg, "name")
	if name == "" {
		name = rel
	}
	manager := nodeManager(dir, pkg)
	detailParts := []string{manager}
	if pkg["workspaces"] != nil {
		detailParts = append(detailParts, "workspaces")
	}
	if exists(filepath.Join(dir, "turbo.json")) {
		detailParts = append(detailParts, "turbo")
	}
	if exists(filepath.Join(dir, "nx.json")) {
		detailParts = append(detailParts, "nx")
	}
	system := buildSystem{
		Directory: rel,
		Manifest:  "package.json",
		Name:      name,
		System:    "Node.js",
		Manager:   manager,
		Detail:    strings.Join(detailParts, " · "),
		Tools:     []string{"node", manager},
		Commands: []buildCommand{
			{Label: "Install", Command: manager + " install", Source: "package manager"},
		},
	}
	scripts := objectField(pkg, "scripts")
	seen := map[string]bool{}
	for _, script := range []string{"dev", "start", "build", "test", "lint", "typecheck", "check", "format", "clean"} {
		if _, ok := scripts[script]; !ok {
			continue
		}
		seen[script] = true
		command := managerRun(manager, script)
		system.Commands = append(system.Commands, buildCommand{
			Label:   "Run " + script,
			Command: command,
			Source:  "scripts." + script,
		})
		system.Targets = append(system.Targets, buildTarget{
			Name:    script,
			Command: command,
			Source:  "package.json",
		})
	}
	names := make([]string, 0, len(scripts))
	for name := range scripts {
		if !seen[name] {
			names = append(names, name)
		}
	}
	sort.Strings(names)
	for _, script := range names {
		if len(system.Targets) >= 20 {
			break
		}
		system.Targets = append(system.Targets, buildTarget{
			Name:    script,
			Command: managerRun(manager, script),
			Source:  "package.json",
		})
	}
	return system
}

func cargoBuildSystem(dir, rel string) buildSystem {
	content := readText(filepath.Join(dir, "Cargo.toml"), 512*1024)
	name := tomlValue(content, "package", "name")
	detail := "package"
	if strings.Contains(content, "[workspace]") {
		detail = "workspace"
		if name == "" {
			name = "workspace"
		}
	}
	if name == "" {
		name = rel
	}
	system := buildSystem{
		Directory: rel,
		Manifest:  "Cargo.toml",
		Name:      name,
		System:    "Rust",
		Manager:   "Cargo",
		Detail:    detail,
		Tools:     []string{"cargo", "rustc", "rustfmt"},
		Commands: []buildCommand{
			{Label: "Check", Command: "cargo check", Source: "Cargo"},
			{Label: "Build", Command: "cargo build", Source: "Cargo"},
			{Label: "Release build", Command: "cargo build --release", Source: "Cargo"},
			{Label: "Test", Command: "cargo test", Source: "Cargo"},
			{Label: "Lint", Command: "cargo clippy --all-targets", Source: "Cargo"},
			{Label: "Format check", Command: "cargo fmt --check", Source: "Cargo"},
		},
	}
	if exists(filepath.Join(dir, "src", "main.rs")) {
		system.Commands = append(system.Commands, buildCommand{
			Label: "Run", Command: "cargo run", Source: "src/main.rs",
		})
	}
	active := false
	for _, line := range strings.Split(content, "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "[[bin]]" {
			active = true
			continue
		}
		if strings.HasPrefix(trimmed, "[") {
			active = false
			continue
		}
		if !active || len(system.Targets) >= 20 || !strings.HasPrefix(trimmed, "name") {
			continue
		}
		rest := strings.TrimSpace(strings.TrimPrefix(trimmed, "name"))
		if !strings.HasPrefix(rest, "=") {
			continue
		}
		bin := strings.Trim(strings.TrimSpace(strings.TrimPrefix(rest, "=")), "\"'")
		if bin != "" {
			system.Targets = append(system.Targets, buildTarget{
				Name: bin, Command: "cargo run --bin " + bin, Source: "[[bin]]",
			})
		}
	}
	return system
}

func goBuildSystem(dir, rel string) buildSystem {
	content := readText(filepath.Join(dir, "go.mod"), 256*1024)
	name := rel
	detail := "modules"
	for _, line := range strings.Split(content, "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "module ") {
			name = strings.TrimSpace(strings.TrimPrefix(trimmed, "module "))
		}
		if strings.HasPrefix(trimmed, "go ") {
			detail = strings.TrimSpace(trimmed)
		}
	}
	system := buildSystem{
		Directory: rel,
		Manifest:  "go.mod",
		Name:      name,
		System:    "Go",
		Manager:   "Go modules",
		Detail:    detail,
		Tools:     []string{"go", "gofmt"},
		Commands: []buildCommand{
			{Label: "Download", Command: "go mod download", Source: "go.mod"},
			{Label: "Build", Command: "go build ./...", Source: "Go"},
			{Label: "Test", Command: "go test ./...", Source: "Go"},
			{Label: "Vet", Command: "go vet ./...", Source: "Go"},
			{Label: "Format check", Command: "gofmt -l .", Source: "Go"},
		},
	}
	if exists(filepath.Join(dir, "main.go")) {
		system.Commands = append(system.Commands, buildCommand{
			Label: "Run", Command: "go run .", Source: "main.go",
		})
	}
	return system
}

func parseMakeTargets(content string) []string {
	targets := make([]string, 0, 20)
	seen := map[string]bool{}
	for _, line := range strings.Split(content, "\n") {
		if line == "" || strings.HasPrefix(line, "\t") || strings.HasPrefix(line, "#") || strings.HasPrefix(line, ".") {
			continue
		}
		index := strings.Index(line, ":")
		if index <= 0 || (index+1 < len(line) && (line[index+1] == '=' || line[index+1] == ':')) {
			continue
		}
		name := strings.TrimSpace(line[:index])
		if name == "" || strings.ContainsAny(name, " \t=%$") || seen[name] {
			continue
		}
		seen[name] = true
		targets = append(targets, name)
		if len(targets) >= 20 {
			break
		}
	}
	return targets
}

func parseTaskTargets(content string) []string {
	targets := make([]string, 0, 20)
	seen := map[string]bool{}
	inTasks := false
	tasksIndent := 0
	for _, line := range strings.Split(content, "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		indent := indentWidth(line)
		if trimmed == "tasks:" {
			inTasks = true
			tasksIndent = indent
			continue
		}
		if !inTasks {
			continue
		}
		if indent <= tasksIndent {
			inTasks = false
			continue
		}
		if indent != tasksIndent+2 {
			continue
		}
		index := strings.Index(trimmed, ":")
		if index <= 0 {
			continue
		}
		name := strings.Trim(strings.TrimSpace(trimmed[:index]), "\"'")
		if name == "" || strings.Contains(name, " ") || seen[name] {
			continue
		}
		seen[name] = true
		targets = append(targets, name)
		if len(targets) >= 20 {
			break
		}
	}
	return targets
}

func parseJustRecipes(content string) []string {
	recipes := make([]string, 0, 20)
	seen := map[string]bool{}
	for _, line := range strings.Split(content, "\n") {
		if line == "" || strings.HasPrefix(line, " ") || strings.HasPrefix(line, "\t") || strings.HasPrefix(line, "#") {
			continue
		}
		if strings.HasPrefix(line, "set ") || strings.HasPrefix(line, "alias ") || strings.HasPrefix(line, "export ") {
			continue
		}
		index := strings.Index(line, ":")
		if index <= 0 || strings.Contains(line, ":=") || strings.Contains(line[:index], "=") {
			continue
		}
		fields := strings.Fields(line[:index])
		if len(fields) == 0 {
			continue
		}
		name := fields[0]
		if strings.HasPrefix(name, "_") || strings.HasPrefix(name, "@") || seen[name] {
			continue
		}
		seen[name] = true
		recipes = append(recipes, name)
		if len(recipes) >= 20 {
			break
		}
	}
	return recipes
}

func pythonBuildSystem(dir, rel string, names map[string]bool) (buildSystem, bool) {
	manifest := ""
	detail := ""
	manager := "pip"
	content := ""
	if names["pyproject.toml"] {
		manifest = "pyproject.toml"
		content = readText(filepath.Join(dir, manifest), 512*1024)
		if exists(filepath.Join(dir, "uv.lock")) {
			manager = "uv"
		} else if exists(filepath.Join(dir, "poetry.lock")) || strings.Contains(content, "[tool.poetry]") {
			manager = "poetry"
		}
		detail = manager
		if names["requirements.txt"] {
			detail += " · requirements.txt"
		}
	} else if names["setup.py"] {
		manifest = "setup.py"
		detail = "setuptools"
	} else if names["requirements.txt"] {
		manifest = "requirements.txt"
		detail = "requirements"
	} else {
		return buildSystem{}, false
	}
	name := tomlValue(content, "project", "name")
	if name == "" {
		name = tomlValue(content, "tool.poetry", "name")
	}
	if name == "" {
		name = rel
	}
	tools := []string{"python3"}
	if manager == "uv" || manager == "poetry" {
		tools = append(tools, manager)
	}
	system := buildSystem{
		Directory: rel,
		Manifest:  manifest,
		Name:      name,
		System:    "Python",
		Manager:   manager,
		Detail:    detail,
		Tools:     tools,
	}
	switch manager {
	case "uv":
		system.Commands = []buildCommand{
			{Label: "Sync", Command: "uv sync", Source: "uv.lock"},
			{Label: "Test", Command: "uv run pytest", Source: "uv"},
			{Label: "Build", Command: "uv build", Source: "uv"},
		}
	case "poetry":
		system.Commands = []buildCommand{
			{Label: "Install", Command: "poetry install", Source: "poetry.lock"},
			{Label: "Test", Command: "poetry run pytest", Source: "poetry"},
			{Label: "Build", Command: "poetry build", Source: "poetry"},
		}
	default:
		if manifest == "requirements.txt" {
			system.Commands = []buildCommand{
				{Label: "Install", Command: "python3 -m pip install -r requirements.txt", Source: manifest},
				{Label: "Test", Command: "python3 -m pytest", Source: "pytest"},
			}
		} else {
			system.Commands = []buildCommand{
				{Label: "Install", Command: "python3 -m pip install -e .", Source: manifest},
				{Label: "Test", Command: "python3 -m pytest", Source: "pytest"},
				{Label: "Build", Command: "python3 -m build", Source: "build"},
			}
		}
	}
	for _, script := range tomlSectionKeys(content, "project.scripts") {
		if len(system.Targets) >= 20 {
			break
		}
		system.Targets = append(system.Targets, buildTarget{
			Name: script, Command: "python3 -m " + script, Source: "project.scripts",
		})
	}
	return system, true
}

func xmlTag(content, tag string) string {
	start := strings.Index(content, "<"+tag+">")
	if start < 0 {
		return ""
	}
	start += len(tag) + 2
	end := strings.Index(content[start:], "</"+tag+">")
	if end < 0 {
		return ""
	}
	return strings.TrimSpace(content[start : start+end])
}

func jvmBuildSystem(dir, rel string, names map[string]bool) []buildSystem {
	var systems []buildSystem
	if names["pom.xml"] {
		content := readText(filepath.Join(dir, "pom.xml"), 512*1024)
		name := xmlTag(content, "artifactId")
		if name == "" {
			name = rel
		}
		systems = append(systems, buildSystem{
			Directory: rel,
			Manifest:  "pom.xml",
			Name:      name,
			System:    "Java",
			Manager:   "Maven",
			Detail:    "pom",
			Tools:     []string{"mvn", "java"},
			Commands: []buildCommand{
				{Label: "Test", Command: "mvn test", Source: "Maven"},
				{Label: "Package", Command: "mvn package", Source: "Maven"},
				{Label: "Verify", Command: "mvn verify", Source: "Maven"},
				{Label: "Clean", Command: "mvn clean", Source: "Maven"},
			},
		})
	}
	gradleFile := ""
	if names["build.gradle.kts"] {
		gradleFile = "build.gradle.kts"
	} else if names["build.gradle"] {
		gradleFile = "build.gradle"
	}
	if gradleFile != "" {
		prefix := "gradle"
		tools := []string{"java", "gradle"}
		if exists(filepath.Join(dir, "gradlew")) {
			prefix = "./gradlew"
			tools = []string{"java"}
		}
		systems = append(systems, buildSystem{
			Directory: rel,
			Manifest:  gradleFile,
			Name:      rel,
			System:    "Java/Kotlin",
			Manager:   "Gradle",
			Detail:    prefix,
			Tools:     tools,
			Commands: []buildCommand{
				{Label: "Build", Command: prefix + " build", Source: "Gradle"},
				{Label: "Test", Command: prefix + " test", Source: "Gradle"},
				{Label: "Clean", Command: prefix + " clean", Source: "Gradle"},
			},
		})
	}
	return systems
}

func detectDirectory(root, dir string) []buildSystem {
	rel := relativeDir(root, dir)
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	names := make(map[string]bool, len(entries))
	for _, entry := range entries {
		names[entry.Name()] = true
	}
	var systems []buildSystem
	if names["package.json"] {
		systems = append(systems, nodeBuildSystem(dir, rel))
	}
	if names["Cargo.toml"] {
		systems = append(systems, cargoBuildSystem(dir, rel))
	}
	if names["go.mod"] {
		systems = append(systems, goBuildSystem(dir, rel))
	}
	if system, ok := pythonBuildSystem(dir, rel, names); ok {
		systems = append(systems, system)
	}
	systems = append(systems, jvmBuildSystem(dir, rel, names)...)
	if names["CMakeLists.txt"] {
		systems = append(systems, buildSystem{
			Directory: rel,
			Manifest:  "CMakeLists.txt",
			Name:      rel,
			System:    "C/C++",
			Manager:   "CMake",
			Detail:    "cmake",
			Tools:     []string{"cmake", "ctest"},
			Commands: []buildCommand{
				{Label: "Configure", Command: "cmake -S . -B build", Source: "CMake"},
				{Label: "Build", Command: "cmake --build build", Source: "CMake"},
				{Label: "Test", Command: "ctest --test-dir build --output-on-failure", Source: "CTest"},
			},
		})
	}
	makeFile := ""
	for _, candidate := range []string{"Makefile", "makefile", "GNUmakefile"} {
		if names[candidate] {
			makeFile = candidate
			break
		}
	}
	if makeFile != "" {
		content := readText(filepath.Join(dir, makeFile), 256*1024)
		targets := parseMakeTargets(content)
		system := buildSystem{
			Directory: rel,
			Manifest:  makeFile,
			Name:      rel,
			System:    "Make",
			Manager:   "make",
			Detail:    fmt.Sprintf("%d target(s)", len(targets)),
			Tools:     []string{"make"},
		}
		if len(targets) > 0 {
			system.Commands = append(system.Commands, buildCommand{
				Label: "Run default", Command: "make", Source: makeFile,
			})
		}
		for _, target := range targets {
			system.Targets = append(system.Targets, buildTarget{
				Name: target, Command: "make " + target, Source: makeFile,
			})
		}
		systems = append(systems, system)
	}
	taskFile := ""
	for _, candidate := range []string{"Taskfile.yml", "Taskfile.yaml", "taskfile.yml", "taskfile.yaml"} {
		if names[candidate] {
			taskFile = candidate
			break
		}
	}
	if taskFile != "" {
		content := readText(filepath.Join(dir, taskFile), 256*1024)
		targets := parseTaskTargets(content)
		system := buildSystem{
			Directory: rel,
			Manifest:  taskFile,
			Name:      rel,
			System:    "Task",
			Manager:   "task",
			Detail:    fmt.Sprintf("%d task(s)", len(targets)),
			Tools:     []string{"task"},
			Commands:  []buildCommand{{Label: "List", Command: "task --list", Source: taskFile}},
		}
		if len(targets) > 0 {
			system.Commands = append(system.Commands, buildCommand{
				Label: "Run default", Command: "task", Source: taskFile,
			})
		}
		for _, target := range targets {
			system.Targets = append(system.Targets, buildTarget{
				Name: target, Command: "task " + target, Source: taskFile,
			})
		}
		systems = append(systems, system)
	}
	justFile := ""
	for _, candidate := range []string{"justfile", "Justfile", ".justfile"} {
		if names[candidate] {
			justFile = candidate
			break
		}
	}
	if justFile != "" {
		content := readText(filepath.Join(dir, justFile), 256*1024)
		recipes := parseJustRecipes(content)
		system := buildSystem{
			Directory: rel,
			Manifest:  justFile,
			Name:      rel,
			System:    "Just",
			Manager:   "just",
			Detail:    fmt.Sprintf("%d recipe(s)", len(recipes)),
			Tools:     []string{"just"},
			Commands:  []buildCommand{{Label: "List", Command: "just --list", Source: justFile}},
		}
		for _, recipe := range recipes {
			system.Targets = append(system.Targets, buildTarget{
				Name: recipe, Command: "just " + recipe, Source: justFile,
			})
		}
		systems = append(systems, system)
	}
	if names["composer.json"] {
		content := readText(filepath.Join(dir, "composer.json"), 512*1024)
		var composer map[string]any
		_ = json.Unmarshal([]byte(content), &composer)
		name := stringField(composer, "name")
		if name == "" {
			name = rel
		}
		system := buildSystem{
			Directory: rel,
			Manifest:  "composer.json",
			Name:      name,
			System:    "PHP",
			Manager:   "Composer",
			Detail:    "composer",
			Tools:     []string{"composer", "php"},
			Commands:  []buildCommand{{Label: "Install", Command: "composer install", Source: "Composer"}},
		}
		scripts := objectField(composer, "scripts")
		scriptNames := make([]string, 0, len(scripts))
		for script := range scripts {
			scriptNames = append(scriptNames, script)
		}
		sort.Strings(scriptNames)
		for _, script := range scriptNames {
			if len(system.Targets) >= 20 {
				break
			}
			system.Targets = append(system.Targets, buildTarget{
				Name: script, Command: "composer run " + script, Source: "composer.json",
			})
		}
		systems = append(systems, system)
	}
	if names["Gemfile"] {
		systems = append(systems, buildSystem{
			Directory: rel,
			Manifest:  "Gemfile",
			Name:      rel,
			System:    "Ruby",
			Manager:   "Bundler",
			Detail:    "bundler",
			Tools:     []string{"bundle", "ruby"},
			Commands: []buildCommand{
				{Label: "Install", Command: "bundle install", Source: "Bundler"},
				{Label: "Default task", Command: "bundle exec rake", Source: "Rake"},
				{Label: "Test", Command: "bundle exec rspec", Source: "RSpec"},
			},
		})
	}
	denoFile := ""
	if names["deno.json"] {
		denoFile = "deno.json"
	} else if names["deno.jsonc"] {
		denoFile = "deno.jsonc"
	}
	if denoFile != "" {
		content := readText(filepath.Join(dir, denoFile), 256*1024)
		var deno map[string]any
		_ = json.Unmarshal([]byte(content), &deno)
		tasks := objectField(deno, "tasks")
		taskNames := make([]string, 0, len(tasks))
		for task := range tasks {
			taskNames = append(taskNames, task)
		}
		sort.Strings(taskNames)
		system := buildSystem{
			Directory: rel,
			Manifest:  denoFile,
			Name:      rel,
			System:    "Deno",
			Manager:   "deno",
			Detail:    "deno",
			Tools:     []string{"deno"},
			Commands:  []buildCommand{{Label: "Test", Command: "deno test", Source: "Deno"}},
		}
		for _, task := range taskNames {
			if len(system.Targets) >= 20 {
				break
			}
			system.Targets = append(system.Targets, buildTarget{
				Name: task, Command: "deno task " + task, Source: denoFile,
			})
		}
		systems = append(systems, system)
	}
	dotnetFile := ""
	for _, entry := range entries {
		name := entry.Name()
		if strings.HasSuffix(name, ".sln") {
			dotnetFile = name
			break
		}
		if dotnetFile == "" && strings.HasSuffix(name, ".csproj") {
			dotnetFile = name
		}
	}
	if dotnetFile != "" {
		systems = append(systems, buildSystem{
			Directory: rel,
			Manifest:  dotnetFile,
			Name:      rel,
			System:    ".NET",
			Manager:   "dotnet",
			Detail:    "dotnet",
			Tools:     []string{"dotnet"},
			Commands: []buildCommand{
				{Label: "Build", Command: "dotnet build", Source: ".NET"},
				{Label: "Test", Command: "dotnet test", Source: ".NET"},
				{Label: "Run", Command: "dotnet run", Source: ".NET"},
			},
		})
	}
	compose := names["compose.yaml"] || names["compose.yml"] || names["docker-compose.yml"] || names["docker-compose.yaml"]
	if names["Dockerfile"] || compose {
		manifest := "Dockerfile"
		if !names["Dockerfile"] {
			manifest = "compose"
		}
		commands := []buildCommand{}
		if names["Dockerfile"] {
			image := strings.ReplaceAll(rel, "/", "-")
			if image == "." {
				image = filepath.Base(root)
			}
			commands = append(commands, buildCommand{
				Label: "Build image", Command: "docker build -t " + image + " .", Source: "Dockerfile",
			})
		}
		if compose {
			commands = append(commands, buildCommand{
				Label: "Compose up", Command: "docker compose up --build", Source: "compose",
			})
		}
		systems = append(systems, buildSystem{
			Directory: rel,
			Manifest:  manifest,
			Name:      rel,
			System:    "Container",
			Manager:   "Docker",
			Detail:    "docker",
			Tools:     []string{"docker"},
			Commands:  commands,
		})
	}
	return systems
}

func detectBuildSystems(root string) ([]buildSystem, []string) {
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return nil, []string{"The active project root is not a readable directory."}
	}
	var dirs []string
	var warnings []string
	err = filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return nil
		}
		if !entry.IsDir() {
			return nil
		}
		if path != root {
			if buildSkipDirs[entry.Name()] {
				return fs.SkipDir
			}
			rel, relErr := filepath.Rel(root, path)
			if relErr == nil && strings.Count(rel, string(filepath.Separator)) > 4 {
				return fs.SkipDir
			}
		}
		dirs = append(dirs, path)
		return nil
	})
	if err != nil {
		warnings = append(warnings, fmt.Sprintf("workspace walk: %v", err))
	}
	systems := make([]buildSystem, 0, 16)
	for _, dir := range dirs {
		for _, system := range detectDirectory(root, dir) {
			if len(systems) >= maxBuildSystems {
				warnings = append(warnings, fmt.Sprintf("build detection stops after %d projects", maxBuildSystems))
				return systems, warnings
			}
			systems = append(systems, system)
		}
	}
	if len(systems) == 0 {
		warnings = append(warnings, "No package or build manifest was found under the active project.")
	}
	return systems, warnings
}

func toolStatus(name string) (bool, string) {
	if _, err := exec.LookPath(name); err != nil {
		return false, "not found"
	}
	if name == "gofmt" {
		return true, "installed"
	}
	args := []string{"--version"}
	switch name {
	case "go":
		args = []string{"version"}
	case "java", "mvn":
		args = []string{"-version"}
	case "gradle":
		args = []string{"--version"}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 1500*time.Millisecond)
	defer cancel()
	output, err := exec.CommandContext(ctx, name, args...).CombinedOutput()
	text := strings.TrimSpace(string(output))
	line := ""
	for _, candidate := range strings.Split(text, "\n") {
		if strings.TrimSpace(candidate) != "" {
			line = strings.TrimSpace(candidate)
			break
		}
	}
	if err != nil {
		if line == "" {
			line = "not runnable"
		}
		return false, compact(line, 100)
	}
	if line == "" {
		return true, "installed"
	}
	return true, compact(line, 100)
}

func buildTools(systems []buildSystem) []toolInfo {
	needed := map[string][]string{}
	for _, system := range systems {
		for _, tool := range system.Tools {
			needed[tool] = append(needed[tool], system.System)
		}
	}
	names := make([]string, 0, len(needed))
	for name := range needed {
		names = append(names, name)
	}
	sort.Strings(names)
	tools := make([]toolInfo, 0, len(names))
	for _, name := range names {
		available, version := toolStatus(name)
		tools = append(tools, toolInfo{
			Name:      name,
			Available: available,
			Version:   version,
			NeededBy:  strings.Join(uniqueStrings(needed[name]), ", "),
		})
	}
	return tools
}

func refreshBuild(state *State) {
	if state.Project == "" {
		state.Build = nil
		state.BuildTools = nil
		state.BuildWarnings = []string{"No active project."}
		state.BuildRoot = ""
		state.BuildScanned = ""
		state.BuildMS = 0
		return
	}
	started := time.Now()
	systems, warnings := detectBuildSystems(state.Project)
	state.Build = systems
	state.BuildWarnings = warnings
	state.BuildTools = buildTools(systems)
	state.BuildRoot = state.Project
	state.BuildScanned = time.Now().Format("15:04:05")
	state.BuildMS = time.Since(started).Milliseconds()
}

func sample(state *State) {
	started := time.Now()
	result := snapshot{
		States:    map[string]int{},
		Hostname:  hostname(),
		OS:        runtime.GOOS,
		Arch:      runtime.GOARCH,
		CPUs:      runtime.NumCPU(),
		Project:   state.Project,
		SampledAt: time.Now().Format("15:04:05"),
	}
	var msg string
	result.Processes, msg = parseProcesses()
	if msg != "" {
		result.Errors = append(result.Errors, msg)
	}
	result.Connections, result.Listeners, result.States, msg = network()
	if msg != "" {
		result.Errors = append(result.Errors, msg)
	}
	result.SampledMS = time.Since(started).Milliseconds()
	state.Snapshot = &result
}

func (s *snapshot) filteredProcesses(state *State) []process {
	filter := strings.ToLower(strings.TrimSpace(state.Filter))
	rows := make([]process, 0, len(s.Processes))
	for _, item := range s.Processes {
		if filter != "" &&
			!strings.Contains(strings.ToLower(item.Command), filter) &&
			!strings.Contains(strings.ToLower(item.Args), filter) &&
			!strings.Contains(strconv.Itoa(item.PID), filter) {
			continue
		}
		rows = append(rows, item)
	}
	switch state.Sort {
	case 1:
		sort.Slice(rows, func(i, j int) bool {
			if rows[i].Memory == rows[j].Memory {
				return rows[i].RSS > rows[j].RSS
			}
			return rows[i].Memory > rows[j].Memory
		})
	case 2:
		sort.Slice(rows, func(i, j int) bool {
			return strings.ToLower(rows[i].Command) < strings.ToLower(rows[j].Command)
		})
	case 3:
		sort.Slice(rows, func(i, j int) bool { return rows[i].PID < rows[j].PID })
	default:
		sort.Slice(rows, func(i, j int) bool {
			if rows[i].CPU == rows[j].CPU {
				return rows[i].Memory > rows[j].Memory
			}
			return rows[i].CPU > rows[j].CPU
		})
	}
	return rows
}

func stateRows(states map[string]int) [][]string {
	type stateCount struct {
		name  string
		count int
	}
	items := make([]stateCount, 0, len(states))
	for name, count := range states {
		if count > 0 {
			items = append(items, stateCount{name, count})
		}
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].count == items[j].count {
			return items[i].name < items[j].name
		}
		return items[i].count > items[j].count
	})
	rows := make([][]string, 0, min(len(items), 16))
	for _, item := range items {
		if len(rows) >= 16 {
			break
		}
		rows = append(rows, []string{item.name, strconv.Itoa(item.count)})
	}
	return rows
}

func buildTab(state *State) map[string]any {
	if state.Project == "" {
		return map[string]any{
			"kind": "column",
			"gap":  12,
			"children": []any{
				text("No active project.", "body", "muted"),
				button("Refresh", "refresh_build", false),
			},
		}
	}
	if len(state.Build) == 0 {
		children := []any{
			text("No build system detected.", "body", "muted"),
			button("Refresh", "refresh_build", false),
		}
		for _, warning := range state.BuildWarnings {
			children = append(children, text(warning, "small", "warning"))
		}
		return map[string]any{"kind": "column", "gap": 12, "children": children}
	}
	selected := state.Build[0]
	projectRows := make([][]string, 0, len(state.Build))
	for _, system := range state.Build {
		projectRows = append(projectRows, []string{
			system.Directory,
			system.System,
			system.Manager,
			system.Manifest,
		})
	}
	commandRows := make([][]string, 0, len(selected.Commands))
	for _, command := range selected.Commands {
		commandRows = append(commandRows, []string{command.Label, command.Command, command.Source})
	}
	targetRows := make([][]string, 0, len(selected.Targets))
	for _, target := range selected.Targets {
		targetRows = append(targetRows, []string{target.Name, target.Command, target.Source})
	}
	toolRows := make([][]string, 0, len(state.BuildTools))
	for _, tool := range state.BuildTools {
		status := "missing"
		if tool.Available {
			status = "available"
		}
		toolRows = append(toolRows, []string{tool.Name, status, tool.Version, tool.NeededBy})
	}
	summaryRows := [][]string{
		{"Directory", selected.Directory},
		{"Manifest", selected.Manifest},
		{"Project", selected.Name},
		{"System", selected.System},
		{"Manager", selected.Manager},
		{"Detail", selected.Detail},
	}
	targetContent := []any{}
	if len(targetRows) > 0 {
		targetContent = append(targetContent, table(
			[]string{"Name", "Command", "Source"},
			targetRows,
			1.0, 2.4, 1.2,
		))
	} else {
		targetContent = append(targetContent, text("No named targets.", "body", "muted"))
	}
	children := []any{
		map[string]any{"kind": "row", "gap": 8, "children": []any{
			button("Refresh", "refresh_build", false),
		}},
		text(fmt.Sprintf("%d project(s) · %s · scanned %s · %d ms", len(state.Build), selected.Directory, state.BuildScanned, state.BuildMS), "caption", "muted"),
		section("Projects", []any{
			table(
				[]string{"Directory", "System", "Manager", "Manifest"},
				projectRows,
				2.4, 1.0, 1.0, 1.2,
			),
		}),
		section("Selected", []any{
			table([]string{"Field", "Value"}, summaryRows, 1.0, 3.0),
		}),
		section("Commands", []any{
			table(
				[]string{"Action", "Command", "Source"},
				commandRows,
				1.0, 2.8, 1.2,
			),
		}),
		section("Targets", targetContent),
		section("Tools", []any{
			table(
				[]string{"Tool", "Status", "Version", "Needed by"},
				toolRows,
				1.0, 1.0, 3.0, 2.0,
			),
		}),
	}
	if len(state.BuildWarnings) > 0 {
		warnings := make([]any, 0, len(state.BuildWarnings))
		for _, warning := range state.BuildWarnings {
			warnings = append(warnings, text(warning, "small", "warning"))
		}
		children = append(children, section("Warnings", warnings))
	}
	return map[string]any{"kind": "column", "gap": 18, "children": children}
}

func processesTab(s *snapshot, state *State) map[string]any {
	rows := s.filteredProcesses(state)
	tableRows := make([][]string, 0, min(len(rows), 100))
	for _, item := range rows {
		if len(tableRows) >= 100 {
			break
		}
		tableRows = append(tableRows, []string{
			strconv.Itoa(item.PID),
			strconv.Itoa(item.PPID),
			fmt.Sprintf("%.1f", item.CPU),
			fmt.Sprintf("%.1f", item.Memory),
			humanBytes(item.RSS),
			item.State,
			processCommand(item),
		})
	}
	return map[string]any{
		"kind": "column",
		"gap":  12,
		"children": []any{
			map[string]any{"kind": "row", "gap": 8, "children": []any{
				button("Refresh", "refresh", false),
			}},
			map[string]any{
				"kind":        "input",
				"label":       "Process filter",
				"value":       state.Filter,
				"placeholder": "name, argument, or PID",
				"handler":     "filter",
			},
			text(fmt.Sprintf("%d of %d process(es)", min(len(rows), 100), len(rows)), "caption", "muted"),
			table(
				[]string{"PID", "PPID", "CPU %", "MEM %", "RSS", "State", "Command"},
				tableRows,
				0.6, 0.6, 0.7, 0.7, 0.8, 0.8, 4.2,
			),
		},
	}
}

func processCommand(item process) string {
	if item.Args != "" {
		return compact(item.Args, 100)
	}
	return compact(item.Command, 100)
}

func processSummary(item process) string {
	fields := strings.Fields(item.Args)
	if len(fields) > 1 {
		command := filepath.Base(fields[0])
		last := filepath.Base(fields[len(fields)-1])
		return compact(command+" "+last, 56)
	}
	if item.Args != "" {
		return compact(item.Args, 56)
	}
	return compact(item.Command, 56)
}

func projectTargets(s *snapshot, state *State) []process {
	if state.Project == "" {
		return nil
	}
	name := filepath.Base(state.Project)
	targets := make([]process, 0, 12)
	for _, item := range s.Processes {
		if strings.Contains(item.Args, state.Project) ||
			strings.Contains(item.Command, state.Project) ||
			(name != "" && (strings.Contains(item.Args, name) || strings.Contains(item.Command, name))) {
			targets = append(targets, item)
		}
	}
	sort.Slice(targets, func(i, j int) bool {
		if targets[i].CPU == targets[j].CPU {
			return targets[i].PID < targets[j].PID
		}
		return targets[i].CPU > targets[j].CPU
	})
	if len(targets) > 12 {
		targets = targets[:12]
	}
	return targets
}

func selectedTarget(targets []process, state *State) (process, bool) {
	for _, item := range targets {
		if item.PID == state.TargetPID {
			return item, true
		}
	}
	if len(targets) > 0 {
		return targets[0], true
	}
	return process{}, false
}

func debugPage(s *snapshot, state *State) map[string]any {
	children := []any{
		map[string]any{"kind": "row", "gap": 8, "children": []any{
			button("Refresh", "refresh", false),
		}},
	}
	targets := projectTargets(s, state)
	if len(targets) == 0 {
		children = append(children, text("No process is running from this project.", "body", "muted"))
		return map[string]any{"kind": "column", "gap": 12, "children": children}
	}
	selected, ok := selectedTarget(targets, state)
	if !ok {
		return map[string]any{"kind": "column", "gap": 12, "children": children}
	}
	buttons := make([]any, 0, len(targets))
	for _, item := range targets {
		marker := "○"
		if item.PID == selected.PID {
			marker = "●"
		}
		buttons = append(buttons, button(
			fmt.Sprintf("%s PID %d · %s", marker, item.PID, processSummary(item)),
			fmt.Sprintf("target:%d", item.PID),
			false,
		))
	}
	children = append(children, section("Targets", []any{
		map[string]any{"kind": "column", "gap": 4, "children": buttons},
	}))
	detailRows := [][]string{
		{"PID", strconv.Itoa(selected.PID)},
		{"Parent PID", strconv.Itoa(selected.PPID)},
		{"State", selected.State},
		{"CPU", fmt.Sprintf("%.1f%%", selected.CPU)},
		{"Memory", fmt.Sprintf("%.1f%% · %s", selected.Memory, humanBytes(selected.RSS))},
		{"Command", compact(selected.Command, 180)},
		{"Arguments", compact(selected.Args, 240)},
	}
	children = append(children, section("Inspector", []any{
		table([]string{"Field", "Value"}, detailRows, 1.0, 4.0),
	}))
	portRows := make([][]string, 0, 24)
	for _, item := range s.Listeners {
		if item.PID == selected.PID {
			portRows = append(portRows, []string{"listen", item.Protocol, item.Local, item.Remote, "LISTEN"})
		}
	}
	for _, item := range s.Connections {
		if item.PID == selected.PID {
			stateName := item.State
			if stateName == "" {
				stateName = "OPEN"
			}
			portRows = append(portRows, []string{"socket", item.Protocol, item.Local, item.Remote, stateName})
			if len(portRows) >= 24 {
				break
			}
		}
	}
	if len(portRows) > 0 {
		children = append(children, section("Ports", []any{
			table([]string{"Kind", "Proto", "Local", "Remote", "State"}, portRows, 0.7, 0.7, 2.0, 2.0, 1.0),
		}))
	} else {
		children = append(children, section("Ports", []any{
			text("No open sockets.", "body", "muted"),
		}))
	}
	return map[string]any{"kind": "column", "gap": 18, "children": children}
}

func portsPage(s *snapshot, state *State) map[string]any {
	connectionRows := make([][]string, 0, min(len(s.Connections), 120))
	for _, item := range s.Connections {
		if len(connectionRows) >= 120 {
			break
		}
		stateName := item.State
		if stateName == "" {
			stateName = "OPEN"
		}
		connectionRows = append(connectionRows, []string{item.Protocol, item.Local, item.Remote, stateName, item.Process})
	}
	listenerRows := make([][]string, 0, min(len(s.Listeners), 120))
	for _, item := range s.Listeners {
		if len(listenerRows) >= 120 {
			break
		}
		listenerRows = append(listenerRows, []string{item.Protocol, item.Local, item.Port, item.Process})
	}
	return map[string]any{
		"kind": "column",
		"gap":  18,
		"children": []any{
			map[string]any{"kind": "row", "gap": 8, "children": []any{
				button("Refresh", "refresh", false),
			}},
			section("States", []any{
				table([]string{"State", "Count"}, stateRows(s.States), 2.0, 1.0),
			}),
			section("Listening", []any{
				table([]string{"Proto", "Address", "Port", "Process"}, listenerRows, 0.9, 2.0, 0.8, 3.0),
			}),
			section("Connections", []any{
				table([]string{"Proto", "Local", "Remote", "State", "Process"}, connectionRows, 0.8, 2.0, 2.0, 1.0, 2.0),
			}),
		},
	}
}

func tree(state *State) map[string]any {
	s := state.Snapshot
	var body map[string]any
	if state.Tab == 3 {
		body = buildTab(state)
	} else if s == nil {
		body = map[string]any{
			"kind": "column",
			"gap":  12,
			"children": []any{
				text("Waiting for a sample.", "body", "muted"),
				button("Refresh", "refresh", false),
			},
		}
	} else {
		switch state.Tab {
		case 0:
			body = debugPage(s, state)
		case 1:
			body = processesTab(s, state)
		default:
			body = portsPage(s, state)
		}
	}
	content := []any{body}
	if s != nil && len(s.Errors) > 0 {
		errors := make([]any, 0, len(s.Errors))
		for _, item := range s.Errors {
			errors = append(errors, text(item, "small", "warning"))
		}
		content = append(content, section("Warnings", errors))
	}
	page := map[string]any{"kind": "column", "gap": 18, "children": content}
	return map[string]any{
		"kind":      "page_tabs",
		"labels":    []string{"Debug", "Processes", "Ports", "Build"},
		"selected":  state.Tab,
		"handler":   "tab",
		"nav_width": 180,
		"children":  []any{page},
	}
}

func publish(state *State) {
	notify("ui.set", map[string]any{"surface": surface, "tree": tree(state)})
}

func (s *State) handleEvent(handler string, value any) {
	switch handler {
	case "tab":
		if number, ok := value.(float64); ok {
			s.Tab = int(math.Max(0, math.Min(3, number)))
			if s.Tab == 3 && s.BuildRoot != s.Project {
				refreshBuild(s)
			}
		}
	case "refresh_build":
		refreshBuild(s)
	case "filter":
		if value, ok := value.(string); ok {
			s.Filter = value
		}
	case "refresh":
		sample(s)
	default:
		if strings.HasPrefix(handler, "target:") {
			if pid, err := strconv.Atoi(strings.TrimPrefix(handler, "target:")); err == nil {
				s.TargetPID = pid
			}
		}
	}
}

func main() {
	incoming := make(chan map[string]any, 32)
	var reader sync.WaitGroup
	reader.Add(1)
	go func() {
		defer reader.Done()
		scanner := bufio.NewScanner(os.Stdin)
		scanner.Buffer(make([]byte, 64*1024), 2*1024*1024)
		for scanner.Scan() {
			line := strings.TrimSpace(scanner.Text())
			if line == "" {
				continue
			}
			var message map[string]any
			if err := json.Unmarshal([]byte(line), &message); err != nil {
				continue
			}
			incoming <- message
		}
		close(incoming)
	}()

	state := newState()
	requestWorkspace := func() {
		send(map[string]any{
			"jsonrpc": "2.0",
			"id":      state.RequestID,
			"method":  "workspace.info",
			"params":  map[string]any{},
		})
	}

	for {
		message, ok := <-incoming
		if !ok {
			return
		}
		method, _ := message["method"].(string)
		if method == "" {
			if id, ok := message["id"].(float64); ok && int(id) == state.RequestID {
				if result, ok := message["result"].(map[string]any); ok {
					if root, ok := result["root"].(string); ok {
						state.Project = root
						refreshBuild(state)
						sample(state)
						publish(state)
					}
				}
			}
			continue
		}
		switch method {
		case "initialize":
			respond(message["id"], map[string]any{
				"protocol": 1,
				"ui": []any{map[string]any{
					"id":    surface,
					"title": "Debugger",
					"icon":  "bug",
					"order": 3,
				}},
			})
			sample(state)
			publish(state)
			requestWorkspace()
		case "ui.open":
			requestWorkspace()
			sample(state)
			publish(state)
		case "ui.event":
			params, _ := message["params"].(map[string]any)
			if params["surface"] != surface {
				continue
			}
			handler, _ := params["handler"].(string)
			state.handleEvent(handler, params["value"])
			publish(state)
		case "shutdown":
			return
		}
	}
}
