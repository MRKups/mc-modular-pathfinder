# Native UI nodes

`ui.set` receives a bounded `monocode_core::visual::Node` tree. It is data, not
HTML, CSS, or JavaScript. The renderer is native GPUI.

## Layout

```json
{"kind":"column","gap":12,"children":[]}
{"kind":"row","gap":8,"children":[]}
{"kind":"grid","columns":2,"gap":12,"children":[]}
{"kind":"card","gap":10,"children":[]}
{"kind":"page_tabs","labels":["Guide","API"],"selected":0,"handler":"page","nav_width":180,
 "children":[{"kind":"text","text":"Page body","size":"body","color":"ink"}]}
```

`page_tabs` is a table-of-contents rail plus one content child. Set `selected` to
the active label and send a new tree when the user clicks another one.
`nav_width` defaults to `180` and is clamped to `140–360`. The content column is
capped at `800` logical pixels and centered in the page. Keep each page in one
vertical flow; use rows only for compact control groups.

## Content

```json
{"kind":"text","text":"Hello","size":"body","color":"ink"}
{"kind":"stat","label":"Files","value":"1,204","detail":"scanned","color":"accent"}
{"kind":"table","columns":["Path","Lines"],"rows":[["src/main.rs","220"]],"weights":[3.0,1.0]}
{"kind":"chart","chart_type":"bar","labels":["Rust","Go"],"height":220,
 "series":[{"label":"Lines","values":[1200,400],"color":"accent"}]}
{"kind":"gauge","label":"Memory","value":62,"min":0,"max":100,"unit":"%","color":"warning"}
{"kind":"donut","slices":[{"label":"Rust","value":70,"color":"accent"}],"label":"lines","value":"1,200"}
{"kind":"heatmap","values":[[1,0.3],[0.3,1]],"rows":["a","b"],"columns":["a","b"],"color":"accent"}
{"kind":"progress","label":"Checks","value":4,"max":6,"detail":"two warnings","color":"positive"}
```

`chart_type` is `bar`, `line`, or `area`. Text sizes are `caption`, `small`,
`body`, `subtitle`, `title`, `hero`. Colors are `ink`, `muted`, `faint`,
`accent`, `positive`, `negative`, `warning`, and `series1`…`series6`.

Tables fit their container instead of scrolling horizontally. `weights` is
optional and sets relative column widths; it must have one value per column.

## Controls

```json
{"kind":"button","label":"Rescan","handler":"refresh","disabled":false}
{"kind":"toggle","label":"Live","value":true,"handler":"live"}
{"kind":"input","label":"Path","value":"","placeholder":"src/main.rs","handler":"path"}
{"kind":"select","label":"Sort","options":["CPU","Memory"],"selected":0,"handler":"sort"}
{"kind":"slider","label":"Top groups","value":10,"min":3,"max":20,"step":1,"handler":"top"}
{"kind":"tabs","labels":["Overview","Files"],"selected":0,"handler":"tab"}
{"kind":"page_tabs","labels":["Overview","Files"],"selected":0,"handler":"tab","children":[]}
{"kind":"code_editor","label":"Source","value":"console.log(1)","language":"js","height":360,"handler":"source","read_only":false}
```

Event values:

| Control | Value |
|---|---|
| button | `null` |
| toggle | boolean |
| input | string |
| select | selected index |
| slider | number |
| tabs | selected index |
| page_tabs | selected index |
| code_editor | full text |

The handler string is returned unchanged in `ui.event`. It must belong to the
currently rendered tree. Reusing the same handler on several controls is fine.

`code_editor` is a multiline syntax-highlighted field. `language` is a short
extension (`js`, `py`, `rs`, `go`, `json`, `toml`, `yaml`, and so on). Unknown
languages stay plain. Set `read_only` for previews and generated output.

## Limits

- 512 nodes and 16 levels.
- 1 MiB serialized tree; 128 KiB total text.
- 250 table rows, 24 table columns.
- 8 chart series, 512 labels and values per chart.
- 32 heatmap rows, 128 columns.
- 96 canvas or scene objects.

When a tree is invalid, the previous tree stays visible and the editor reports
the validation error.
