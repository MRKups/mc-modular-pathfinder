// A Go plugin for the Monocode task board example.
//
// It owns the task list and speaks newline-delimited JSON-RPC on stdio.
// stdout is protocol traffic only; logs go to stderr.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
)

const surfaceID = "tasks"

var filters = []string{"All", "Open", "Done"}

type task struct {
	Title    string
	Done     bool
	Priority string
}

type state struct {
	tasks  []task
	draft  string
	filter int
}

func newState() *state {
	return &state{
		tasks: []task{
			{Title: "Design the plugin contract", Done: true, Priority: "High"},
			{Title: "Ship the native surface", Done: true, Priority: "High"},
			{Title: "Exercise every control", Done: false, Priority: "Medium"},
			{Title: "Write a third-party plugin", Done: false, Priority: "Low"},
		},
	}
}

func text(value, size, color string) map[string]any {
	return map[string]any{
		"kind":  "text",
		"text":  value,
		"size":  size,
		"color": color,
	}
}

func card(title string, children []any) map[string]any {
	all := []any{text(title, "subtitle", "ink")}
	all = append(all, children...)
	return map[string]any{"kind": "card", "gap": 10, "children": all}
}

func (s *state) counts() (done, open int) {
	for _, item := range s.tasks {
		if item.Done {
			done++
		}
	}
	return done, len(s.tasks) - done
}

func (s *state) visibleTasks() []struct {
	item  task
	index int
} {
	var visible []struct {
		item  task
		index int
	}
	for index, item := range s.tasks {
		if s.filter == 1 && item.Done {
			continue
		}
		if s.filter == 2 && !item.Done {
			continue
		}
		visible = append(visible, struct {
			item  task
			index int
		}{item, index})
	}
	return visible
}

func (s *state) taskRows() []any {
	visible := s.visibleTasks()
	if len(visible) == 0 {
		message := "No tasks yet. Add one above."
		if s.filter != 0 {
			message = "Nothing in this filter."
		}
		return []any{text(message, "body", "faint")}
	}
	rows := make([]any, 0, len(visible))
	for _, entry := range visible {
		rows = append(rows, map[string]any{
			"kind": "row",
			"gap":  10,
			"children": []any{
				map[string]any{
					"kind":    "toggle",
					"label":   fmt.Sprintf("%s  ·  %s", entry.item.Title, entry.item.Priority),
					"value":   entry.item.Done,
					"handler": fmt.Sprintf("toggle:%d", entry.index),
				},
				map[string]any{
					"kind":     "button",
					"label":    "Remove",
					"handler":  fmt.Sprintf("remove:%d", entry.index),
					"disabled": false,
				},
			},
		})
	}
	return rows
}

func (s *state) tree() map[string]any {
	done, open := s.counts()
	completion := 0
	if len(s.tasks) > 0 {
		completion = (done * 100) / len(s.tasks)
	}
	options := make([]any, len(filters))
	for index, filter := range filters {
		options[index] = filter
	}

	return map[string]any{
		"kind": "column",
		"gap":  16,
		"children": []any{
			text("Task board", "hero", "ink"),
			text("The list lives in the Go plugin process. Toggling, filtering and removing are all round-trips through the host.", "body", "muted"),
			map[string]any{
				"kind":    "grid",
				"columns": 3,
				"gap":     12,
				"children": []any{
					map[string]any{"kind": "stat", "label": "Open", "value": strconv.Itoa(open), "detail": "waiting", "color": "warning"},
					map[string]any{"kind": "stat", "label": "Done", "value": strconv.Itoa(done), "detail": "completed", "color": "positive"},
					map[string]any{
						"kind":   "stat",
						"label":  "Completion",
						"value":  fmt.Sprintf("%d%%", completion),
						"detail": fmt.Sprintf("%d of %d", done, len(s.tasks)),
						"color":  "accent",
						"trend":  []float64{0, float64(completion) * 0.25, float64(completion) * 0.6, float64(completion)},
					},
				},
			},
			card("Add a task", []any{
				map[string]any{
					"kind": "row",
					"gap":  10,
					"children": []any{
						map[string]any{
							"kind":        "input",
							"label":       "New task",
							"value":       s.draft,
							"placeholder": "What needs doing?",
							"handler":     "draft",
						},
						map[string]any{
							"kind":     "button",
							"label":    "Add task",
							"handler":  "add",
							"disabled": strings.TrimSpace(s.draft) == "",
						},
					},
				},
				map[string]any{
					"kind": "row",
					"gap":  10,
					"children": []any{
						map[string]any{
							"kind":     "select",
							"label":    "Filter",
							"options":  options,
							"selected": s.filter,
							"handler":  "filter",
						},
						map[string]any{
							"kind":     "button",
							"label":    "Clear completed",
							"handler":  "clear-done",
							"disabled": done == 0,
						},
					},
				},
			}),
			card("Tasks", s.taskRows()),
			map[string]any{
				"kind":   "progress",
				"label":  "Completion",
				"value":  completion,
				"max":    100,
				"detail": fmt.Sprintf("%d open", open),
				"color":  "accent",
			},
		},
	}
}

type plugin struct {
	state  *state
	writer *bufio.Writer
}

func (p *plugin) send(message map[string]any) {
	data, err := json.Marshal(message)
	if err != nil {
		return
	}
	p.writer.Write(data)
	p.writer.WriteByte('\n')
	p.writer.Flush()
}

func (p *plugin) publish() {
	p.send(map[string]any{
		"jsonrpc": "2.0",
		"method":  "ui.set",
		"params": map[string]any{
			"surface": surfaceID,
			"tree":    p.state.tree(),
		},
	})
}

func (p *plugin) handleEvent(params map[string]any) {
	if surface, _ := params["surface"].(string); surface != surfaceID {
		return
	}
	handler, _ := params["handler"].(string)
	value := params["value"]

	switch {
	case handler == "draft":
		if text, ok := value.(string); ok {
			if len(text) > 200 {
				text = text[:200]
			}
			p.state.draft = text
		} else {
			p.state.draft = ""
		}
	case handler == "add":
		title := strings.TrimSpace(p.state.draft)
		if title != "" {
			p.state.tasks = append(p.state.tasks, task{Title: title, Priority: "Medium"})
			p.state.draft = ""
		}
	case handler == "filter":
		if number, ok := value.(float64); ok {
			p.state.filter = int(number)
			if p.state.filter < 0 {
				p.state.filter = 0
			}
			if p.state.filter >= len(filters) {
				p.state.filter = len(filters) - 1
			}
		}
	case handler == "clear-done":
		kept := p.state.tasks[:0]
		for _, item := range p.state.tasks {
			if !item.Done {
				kept = append(kept, item)
			}
		}
		p.state.tasks = kept
	case strings.HasPrefix(handler, "toggle:"):
		if index, err := strconv.Atoi(strings.TrimPrefix(handler, "toggle:")); err == nil && index >= 0 && index < len(p.state.tasks) {
			if done, ok := value.(bool); ok {
				p.state.tasks[index].Done = done
			}
		}
	case strings.HasPrefix(handler, "remove:"):
		if index, err := strconv.Atoi(strings.TrimPrefix(handler, "remove:")); err == nil && index >= 0 && index < len(p.state.tasks) {
			p.state.tasks = append(p.state.tasks[:index], p.state.tasks[index+1:]...)
		}
	}
	p.publish()
}

func (p *plugin) handle(message map[string]any) bool {
	method, _ := message["method"].(string)
	if method == "" {
		return true
	}
	switch method {
	case "initialize":
		p.send(map[string]any{
			"jsonrpc": "2.0",
			"id":      message["id"],
			"result": map[string]any{
				"protocol": 1,
				"ui": []any{
					map[string]any{
						"id":    surfaceID,
						"title": "Tasks",
						"icon":  "note",
						"order": 3,
					},
				},
			},
		})
		p.publish()
	case "ui.open":
		p.publish()
	case "ui.event":
		if params, ok := message["params"].(map[string]any); ok {
			p.handleEvent(params)
		}
	case "shutdown":
		return false
	default:
		if _, ok := message["id"]; ok {
			p.send(map[string]any{
				"jsonrpc": "2.0",
				"id":      message["id"],
				"error": map[string]any{
					"code":    -32601,
					"message": "Unknown method: " + method,
				},
			})
		}
	}
	return true
}

func main() {
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 64*1024), 2*1024*1024)
	writer := bufio.NewWriter(os.Stdout)
	p := &plugin{state: newState(), writer: writer}

	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}
		var message map[string]any
		if err := json.Unmarshal([]byte(line), &message); err != nil {
			fmt.Fprintf(os.Stderr, "tasks plugin: invalid JSON: %v\n", err)
			continue
		}
		if !p.handle(message) {
			return
		}
	}
}
