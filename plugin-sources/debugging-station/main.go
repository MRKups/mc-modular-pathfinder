//! A read-only live debugging station.
//!
//! It samples processes, load, memory, disks, and network sockets with ordinary
//! OS tools, then renders the snapshot through Monocode's native component
//! schema. stdout is protocol traffic only; diagnostics go to stderr.

package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"os/exec"
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
	Project   string
	Snapshot  *snapshot
	Tab       int
	Live      bool
	Sort      int
	Filter    string
	LastScan  time.Time
	Status    string
	RequestID int
}

func newState() *State {
	return &State{
		Tab:       0,
		Live:      true,
		Sort:      0,
		Status:    "Waiting for the first sample…",
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

func card(title string, children []any) map[string]any {
	all := []any{text(title, "subtitle", "ink")}
	all = append(all, children...)
	return map[string]any{"kind": "card", "gap": 10, "children": all}
}

func stat(label, value string, detail string, color string) map[string]any {
	node := map[string]any{
		"kind":  "stat",
		"label": label,
		"value": value,
		"color": color,
	}
	if detail != "" {
		node["detail"] = detail
	}
	return node
}

func table(columns []string, rows [][]string) map[string]any {
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
	return map[string]any{"kind": "table", "columns": columns, "rows": cleanRows}
}

func button(label, handler string, disabled bool) map[string]any {
	return map[string]any{"kind": "button", "label": label, "handler": handler, "disabled": disabled}
}

func toggle(label string, value bool, handler string) map[string]any {
	return map[string]any{"kind": "toggle", "label": label, "value": value, "handler": handler}
}

func selectNode(label string, options []string, selected int, handler string) map[string]any {
	if len(options) == 0 {
		options = []string{"—"}
	}
	if selected < 0 {
		selected = 0
	}
	if selected >= len(options) {
		selected = len(options) - 1
	}
	return map[string]any{
		"kind":     "select",
		"label":    label,
		"options":  options,
		"selected": selected,
		"handler":  handler,
	}
}

func run(name string, args ...string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
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
	result.Load1, result.Load5, result.Load15, msg = loadAverage()
	if msg != "" {
		result.Errors = append(result.Errors, msg)
	}
	result.MemoryTotal, result.MemoryUsed, result.MemoryFree, result.SwapTotal, result.SwapUsed, msg = memory()
	if msg != "" {
		result.Errors = append(result.Errors, msg)
	}
	result.Disks, msg = diskInfo()
	if msg != "" {
		result.Errors = append(result.Errors, msg)
	}
	result.Connections, result.Listeners, result.States, msg = network()
	if msg != "" {
		result.Errors = append(result.Errors, msg)
	}
	result.Uptime = uptime()
	result.SampledMS = time.Since(started).Milliseconds()
	state.Snapshot = &result
	state.LastScan = time.Now()
	state.Status = fmt.Sprintf("Sampled %d processes and %d connections in %d ms",
		len(result.Processes), len(result.Connections), result.SampledMS)
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

func stateSlices(states map[string]int) []map[string]any {
	items := make([]struct {
		name  string
		count int
	}, 0, len(states))
	for name, count := range states {
		if count > 0 {
			items = append(items, struct {
				name  string
				count int
			}{name, count})
		}
	}
	sort.Slice(items, func(i, j int) bool { return items[i].count > items[j].count })
	colors := []string{"accent", "series2", "series4", "series5", "series6", "positive"}
	slices := make([]map[string]any, 0, 6)
	for index, item := range items {
		if index >= 6 {
			break
		}
		slices = append(slices, map[string]any{
			"label": item.name,
			"value": float64(item.count),
			"color": colors[index%len(colors)],
		})
	}
	if len(slices) == 0 {
		slices = append(slices, map[string]any{"label": "No sockets", "value": 1.0, "color": "muted"})
	}
	return slices
}

func overview(s *snapshot, state *State) map[string]any {
	running := 0
	zombie := 0
	for _, item := range s.Processes {
		if strings.HasPrefix(item.State, "R") {
			running++
		}
		if strings.HasPrefix(item.State, "Z") {
			zombie++
		}
	}
	rows := s.filteredProcesses(state)
	top := rows
	if len(top) > 8 {
		top = top[:8]
	}
	processRows := make([][]string, 0, len(top))
	labels := make([]string, 0, len(top))
	values := make([]float64, 0, len(top))
	for _, item := range top {
		command := item.Command
		if len(command) > 34 {
			command = command[:34]
		}
		processRows = append(processRows, []string{
			strconv.Itoa(item.PID),
			fmt.Sprintf("%.1f%%", item.CPU),
			fmt.Sprintf("%.1f%%", item.Memory),
			humanBytes(item.RSS),
			command,
		})
		labels = append(labels, fmt.Sprintf("%d %s", item.PID, command))
		values = append(values, item.CPU)
	}
	if len(labels) == 0 {
		labels = []string{"No processes"}
		values = []float64{0}
	}
	listenerRows := make([][]string, 0, min(len(s.Listeners), 12))
	for _, item := range s.Listeners {
		if len(listenerRows) >= 12 {
			break
		}
		listenerRows = append(listenerRows, []string{item.Protocol, item.Local, item.Port, item.Process})
	}
	memoryPercent := 0.0
	if s.MemoryTotal > 0 {
		memoryPercent = 100 * float64(s.MemoryUsed) / float64(s.MemoryTotal)
	}
	loadMax := math.Max(1, float64(s.CPUs)*2)
	slices := stateSlices(s.States)
	return map[string]any{
		"kind": "column",
		"gap":  12,
		"children": []any{
			map[string]any{
				"kind":    "grid",
				"columns": 4,
				"gap":     12,
				"children": []any{
					stat("Processes", strconv.Itoa(len(s.Processes)), fmt.Sprintf("%d running · %d zombie", running, zombie), "accent"),
					stat("Listening", strconv.Itoa(len(s.Listeners)), "TCP and UDP sockets", "positive"),
					stat("Connections", strconv.Itoa(len(s.Connections)), "non-listening sockets", "series6"),
					stat("Load 1m", fmt.Sprintf("%.2f", s.Load1), fmt.Sprintf("%d CPUs · 5m %.2f", s.CPUs, s.Load5), "warning"),
				},
			},
			map[string]any{
				"kind":    "grid",
				"columns": 2,
				"gap":     12,
				"children": []any{
					card("Top processes by CPU", []any{
						map[string]any{
							"kind":       "chart",
							"chart_type": "bar",
							"labels":     labels,
							"series": []any{map[string]any{
								"label":  "CPU %",
								"values": values,
								"color":  "accent",
							}},
							"height": 240,
						},
					}),
					card("Socket states", []any{
						map[string]any{
							"kind":   "donut",
							"slices": slices,
							"label":  "sockets",
							"value":  strconv.Itoa(len(s.Connections) + len(s.Listeners)),
						},
						map[string]any{
							"kind":   "progress",
							"label":  "Memory used",
							"value":  float64(s.MemoryUsed),
							"max":    math.Max(1, float64(s.MemoryTotal)),
							"detail": fmt.Sprintf("%s of %s", humanBytes(s.MemoryUsed), humanBytes(s.MemoryTotal)),
							"color":  "series2",
						},
					}),
				},
			},
			map[string]any{
				"kind":    "grid",
				"columns": 2,
				"gap":     12,
				"children": []any{
					card("Busiest processes", []any{
						table([]string{"PID", "CPU", "MEM", "RSS", "Command"}, processRows),
					}),
					card("Listening ports", []any{
						table([]string{"Proto", "Address", "Port", "Process"}, listenerRows),
					}),
				},
			},
			map[string]any{
				"kind":    "grid",
				"columns": 2,
				"gap":     12,
				"children": []any{
					card("System", []any{
						map[string]any{
							"kind":  "gauge",
							"label": "Load (1 minute)",
							"value": math.Min(s.Load1, loadMax),
							"min":   0,
							"max":   loadMax,
							"unit":  "load",
							"color": "warning",
						},
						text(fmt.Sprintf("%s · %s/%s · up %s", s.Hostname, s.OS, s.Arch, s.Uptime), "small", "muted"),
					}),
					card("Memory", []any{
						text(fmt.Sprintf("%s used · %s free · %s total", humanBytes(s.MemoryUsed), humanBytes(s.MemoryFree), humanBytes(s.MemoryTotal)), "body", "muted"),
						map[string]any{
							"kind":  "gauge",
							"label": "Memory",
							"value": memoryPercent,
							"min":   0,
							"max":   100,
							"unit":  "%",
							"color": "series2",
						},
					}),
				},
			},
		},
	}
}

func processesTab(s *snapshot, state *State) map[string]any {
	rows := s.filteredProcesses(state)
	tableRows := make([][]string, 0, min(len(rows), 40))
	for _, item := range rows {
		if len(tableRows) >= 40 {
			break
		}
		tableRows = append(tableRows, []string{
			strconv.Itoa(item.PID),
			strconv.Itoa(item.PPID),
			fmt.Sprintf("%.1f", item.CPU),
			fmt.Sprintf("%.1f", item.Memory),
			humanBytes(item.RSS),
			item.State,
			item.Command,
		})
	}
	sortOptions := []string{"CPU", "Memory", "Name", "PID"}
	return map[string]any{
		"kind": "column",
		"gap":  12,
		"children": []any{
			card("Filter", []any{
				map[string]any{"kind": "row", "gap": 8, "children": []any{
					map[string]any{
						"kind":        "input",
						"label":       "Process filter",
						"value":       state.Filter,
						"placeholder": "name, argument, or PID",
						"handler":     "filter",
					},
					selectNode("Sort", sortOptions, state.Sort, "sort"),
					button("Refresh", "refresh", false),
					toggle("Live (2 s)", state.Live, "live"),
				}},
				text(fmt.Sprintf("Showing %d of %d processes", min(len(rows), 40), len(rows)), "caption", "muted"),
			}),
			card("Processes", []any{
				table([]string{"PID", "PPID", "CPU %", "MEM %", "RSS", "State", "Command"}, tableRows),
			}),
		},
	}
}

func networkTab(s *snapshot, state *State) map[string]any {
	connectionRows := make([][]string, 0, min(len(s.Connections), 100))
	for _, item := range s.Connections {
		if len(connectionRows) >= 100 {
			break
		}
		stateName := item.State
		if stateName == "" {
			stateName = "OPEN"
		}
		connectionRows = append(connectionRows, []string{item.Protocol, item.Local, item.Remote, stateName, item.Process})
	}
	listenerRows := make([][]string, 0, min(len(s.Listeners), 100))
	for _, item := range s.Listeners {
		if len(listenerRows) >= 100 {
			break
		}
		listenerRows = append(listenerRows, []string{item.Protocol, item.Local, item.Port, item.Process})
	}
	established := s.States["ESTABLISHED"]
	timeWait := s.States["TIME_WAIT"]
	return map[string]any{
		"kind": "column",
		"gap":  12,
		"children": []any{
			map[string]any{
				"kind":    "grid",
				"columns": 4,
				"gap":     12,
				"children": []any{
					stat("Listening", strconv.Itoa(len(s.Listeners)), "local services", "positive"),
					stat("Established", strconv.Itoa(established), "active TCP peers", "accent"),
					stat("Time wait", strconv.Itoa(timeWait), "closing sockets", "warning"),
					stat("Total sockets", strconv.Itoa(len(s.Connections)+len(s.Listeners)), "sampled now", "series6"),
				},
			},
			map[string]any{
				"kind":    "grid",
				"columns": 2,
				"gap":     12,
				"children": []any{
					card("Socket states", []any{
						map[string]any{
							"kind":   "donut",
							"slices": stateSlices(s.States),
							"label":  "sockets",
							"value":  strconv.Itoa(len(s.Connections) + len(s.Listeners)),
						},
					}),
					card("Listening ports", []any{
						table([]string{"Proto", "Address", "Port", "Process"}, listenerRows),
					}),
				},
			},
			card("Connections (first 100)", []any{
				table([]string{"Proto", "Local", "Remote", "State", "Process"}, connectionRows),
			}),
		},
	}
}

func systemTab(s *snapshot, state *State) map[string]any {
	diskRows := make([][]string, 0, len(s.Disks))
	for _, item := range s.Disks {
		diskRows = append(diskRows, []string{
			item.Mount,
			humanBytes(item.Size),
			humanBytes(item.Used),
			humanBytes(item.Free),
			item.Percent,
		})
	}
	memoryPercent := 0.0
	if s.MemoryTotal > 0 {
		memoryPercent = 100 * float64(s.MemoryUsed) / float64(s.MemoryTotal)
	}
	swap := "not reported"
	if s.SwapTotal > 0 {
		swap = fmt.Sprintf("%s of %s", humanBytes(s.SwapUsed), humanBytes(s.SwapTotal))
	}
	return map[string]any{
		"kind": "column",
		"gap":  12,
		"children": []any{
			map[string]any{
				"kind":    "grid",
				"columns": 4,
				"gap":     12,
				"children": []any{
					stat("Host", s.Hostname, s.OS+"/"+s.Arch, "ink"),
					stat("Uptime", s.Uptime, "since boot", "muted"),
					stat("CPUs", strconv.Itoa(s.CPUs), fmt.Sprintf("load %.2f / %.2f / %.2f", s.Load1, s.Load5, s.Load15), "accent"),
					stat("Swap", swap, "if available", "series6"),
				},
			},
			map[string]any{
				"kind":    "grid",
				"columns": 2,
				"gap":     12,
				"children": []any{
					card("Memory and load", []any{
						map[string]any{
							"kind":   "progress",
							"label":  "Memory",
							"value":  float64(s.MemoryUsed),
							"max":    math.Max(1, float64(s.MemoryTotal)),
							"detail": fmt.Sprintf("%.1f%% used", memoryPercent),
							"color":  "series2",
						},
						map[string]any{
							"kind":  "gauge",
							"label": "Load (1 minute)",
							"value": math.Min(s.Load1, math.Max(1, float64(s.CPUs)*2)),
							"min":   0,
							"max":   math.Max(1, float64(s.CPUs)*2),
							"unit":  "load",
							"color": "warning",
						},
					}),
					card("Disks", []any{
						table([]string{"Mount", "Size", "Used", "Free", "Use"}, diskRows),
					}),
				},
			},
			card("Sample notes", []any{
				text("Read-only: this plugin never kills processes or changes sockets.", "body", "muted"),
				text(fmt.Sprintf("Sampled at %s in %d ms.", s.SampledAt, s.SampledMS), "caption", "muted"),
			}),
		},
	}
}

func tree(state *State) map[string]any {
	s := state.Snapshot
	if s == nil {
		return map[string]any{
			"kind": "column",
			"gap":  12,
			"children": []any{
				text("Debugging Station", "hero", "ink"),
				text(state.Status, "body", "muted"),
			},
		}
	}
	var body map[string]any
	switch state.Tab {
	case 1:
		body = processesTab(s, state)
	case 2:
		body = networkTab(s, state)
	case 3:
		body = systemTab(s, state)
	default:
		body = overview(s, state)
	}
	var errors []any
	for _, item := range s.Errors {
		errors = append(errors, text("• "+item, "small", "warning"))
	}
	project := s.Project
	if project == "" {
		project = "no active project"
	}
	children := []any{
		text("Debugging Station", "hero", "ink"),
		text(fmt.Sprintf("Live system snapshot · %s · %s", project, s.Hostname), "body", "muted"),
		map[string]any{
			"kind": "row",
			"gap":  8,
			"children": []any{
				toggle("Live (2 s)", state.Live, "live"),
				button("Refresh now", "refresh", false),
				stat("Last sample", s.SampledAt, fmt.Sprintf("%d ms", s.SampledMS), "ink"),
			},
		},
		text(state.Status, "small", "muted"),
	}
	if len(errors) > 0 {
		children = append(children, card("Degraded readings", errors))
	}
	children = append(children,
		map[string]any{
			"kind":     "tabs",
			"labels":   []string{"Overview", "Processes", "Network", "System"},
			"selected": state.Tab,
			"handler":  "tab",
		},
		body,
	)
	return map[string]any{"kind": "column", "gap": 16, "children": children}
}

func publish(state *State) {
	notify("ui.set", map[string]any{"surface": surface, "tree": tree(state)})
}

func (s *State) handleEvent(handler string, value any) {
	switch handler {
	case "tab":
		if number, ok := value.(float64); ok {
			s.Tab = int(math.Max(0, math.Min(3, number)))
		}
	case "live":
		if enabled, ok := value.(bool); ok {
			s.Live = enabled
			s.Status = "Live sampling is on (2 s)"
			if !enabled {
				s.Status = "Live sampling is off"
			}
		}
	case "sort":
		if number, ok := value.(float64); ok {
			s.Sort = int(math.Max(0, math.Min(3, number)))
		}
	case "filter":
		if value, ok := value.(string); ok {
			s.Filter = value
		}
	case "refresh":
		sample(s)
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
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	requestWorkspace := func() {
		send(map[string]any{
			"jsonrpc": "2.0",
			"id":      state.RequestID,
			"method":  "workspace.info",
			"params":  map[string]any{},
		})
	}

	for {
		select {
		case message, ok := <-incoming:
			if !ok {
				return
			}
			method, _ := message["method"].(string)
			if method == "" {
				if id, ok := message["id"].(float64); ok && int(id) == state.RequestID {
					if result, ok := message["result"].(map[string]any); ok {
						if root, ok := result["root"].(string); ok {
							state.Project = root
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
						"title": "Debugging Station",
						"icon":  "terminal",
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
		case <-ticker.C:
			if state.Live && time.Since(state.LastScan) >= 2*time.Second {
				sample(state)
				publish(state)
			}
		}
	}
}
