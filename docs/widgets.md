# Widgets

Every widget, in the order the [Pitot window](control-window.md) lists them. The name is the widget's
`[widgets.<name>]` table in [`config.toml`](configuration.md#format). A widget that is off by default shows
once its switch in the Pitot window is turned on, or `enabled = true` is set in its table.

How a widget is added: [Adding a widget](overlay-rendering.md#adding-a-widget). Each new widget adds one row at
the end of this table (rows added on different branches merge without conflicts).

| Name | In the Pitot window | On by default | Page |
|---|---|---|---|
| `status` | Status | yes | [overlay-rendering.md](overlay-rendering.md#widgets) |
| `inputs` | Input trace | yes | [input-trace.md](input-trace.md) |
| `relative` | Relative | yes | [relative.md](relative.md) |
| `standings` | Standings | yes | [standings.md](standings.md) |
| `fuel` | Fuel | yes | [fuel.md](fuel.md) |
| `pit` | Pit stops | yes | [pit.md](pit.md) |
| `sectors` | Sectors | yes | [sectors.md](sectors.md) |
| `wind` | Wind | yes | [wind.md](wind.md) |
| `tires` | Tires | yes | [tires.md](tires.md) |
| `trackmap` | Track map | yes | [trackmap.md](trackmap.md) |
