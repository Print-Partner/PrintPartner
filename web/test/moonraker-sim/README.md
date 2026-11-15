# Moonraker simulator

`moonraker-sim.mjs` is a stateful Moonraker printer for the daily-loop test
(GRE-1528). It uses only Node's standard library. It answers the Moonraker
paths Print Partner calls and keeps its own printer state, so a print start only
changes that state. It never opens a connection of its own, and no real printer
or print is involved.

```sh
node web/test/moonraker-sim/moonraker-sim.mjs --api-key-file <file> \
  [--host 127.0.0.1] [--port 0] [--control-host 127.0.0.1] [--control-port 0]
```

The first line on stdout is `{"printer_port":N,"control_port":N}`. SIGTERM or
SIGINT stops it. The API key is read from the file and is never printed or
logged. Without `--api-key-file`, every request is accepted. With it, a request
without the matching `X-Api-Key` gets 401.

## Printer port

| Request | Answer |
| --- | --- |
| `GET /server/info` | `klippy_state: ready` |
| `GET /printer/objects/query?...` | `print_stats`, `virtual_sdcard`, `display_status`, `extruder` and `heater_bed` for the current state |
| `POST /server/files/upload` (multipart, part `file`) | 201 with `{result:{item:{path,root:"gcodes"}, action:"create_file"}}`. The file's name, size and SHA-256 are kept; its bytes are dropped. |
| `POST /printer/print/start?filename=` | Starts a file it holds, only from `standby`, `complete` or `cancelled`: state `printing`, progress 0. Otherwise 400. |
| `GET /server/files/list`, `/server/files/directory`, `/machine/proc_stats`, `/server/webcams/list` | Minimal valid answers; the file list shows uploaded names |

States: `standby`, `printing`, `paused`, `complete`, `cancelled`, `error`, and
`offline`, which closes every printer connection without answering, including
one kept alive from before.

## Control port

For the harness only. Run it on host loopback, or publish it only there.

| Request | What it does |
| --- | --- |
| `GET /_sim/log` | State, progress, file name, counts (`requests`, `unauthorized`, `uploads`, `starts`), files by name with size, SHA-256 and upload count, the armed fault, and the last 1,000 requests as method and path. No contents, headers or key. |
| `POST /_sim/state` `{state, progress?, filename?}` | Sets the state. `progress` is 0 to 1. |
| `POST /_sim/fault` `{fault, path?, ms?}` | Arms one fault for the next matching request: `lose-upload` (stores the file, then closes without answering), `lose-start` (starts, then closes), `late` (answers after `ms`, default 70,000), `error-500` (answers 500 and changes nothing). `path` limits `late` and `error-500` to one path. |

`npm --prefix web run test:moonraker-sim` runs its tests. They are part of
`npm --prefix web test`.
