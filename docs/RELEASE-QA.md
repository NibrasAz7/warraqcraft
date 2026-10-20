# Release QA

This checklist is the release-artifact gate for WarraqCraft. Run it on the
same OS as the artifact whenever possible; CI packaging checks do not
replace real desktop smoke testing.

## Artifact expectations

| Platform | Artifact | Required contents | Runtime notes |
| --- | --- | --- | --- |
| Windows | `dist/warraqcraft-<version>-windows-<arch>.zip` | `warraqcraft.exe`, `warraqcraft-cli.exe`, `templates/`, README, licences, NOTICE; `bin/tectonic.exe` when packed with `-IncludeTectonic` | Portable zip; unsigned binaries can trigger SmartScreen. |
| macOS | `dist/warraqcraft-<version>-macos-<arch>.tar.gz`, `.dmg` | tarball plus `WarraqCraft.app`; app resources must include `templates/`, README, licences and NOTICE | Unsigned/unnotarized builds can be blocked by Gatekeeper. |
| Linux | `dist/warraqcraft-<version>-linux-<arch>.tar.gz`, `.deb` | tarball; deb installs binaries to `/usr/bin`, templates to `/usr/share/warraqcraft/templates`, docs to `/usr/share/doc/warraqcraft`, and a desktop entry | `.deb` is an installable package, not a repository package; bundled Tectonic should use the musl Linux asset so it is not tied to the host glibc. |

Template discovery must work in portable, app-bundle and installed Linux
layouts. `WARRAQCRAFT_TEMPLATES` remains the explicit override for tests and
custom packaging.

## Smoke checklist

Run on each OS artifact:

```sh
warraqcraft-cli commands
warraqcraft-cli cmd template.list --params '{}'
warraqcraft-cli cmd project.structure --file path/to/main.tex
warraqcraft --control "$TMPDIR/warraqcraft-control.json" path/to/main.tex
warraqcraft-cli ui --control "$TMPDIR/warraqcraft-control.json" source.get
warraqcraft-cli ui --control "$TMPDIR/warraqcraft-control.json" document.disk_state
warraqcraft-cli ui --control "$TMPDIR/warraqcraft-control.json" document.compile
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"release-qa","version":"1"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | warraqcraft-cli mcp
```

Manual GUI pass:

1. Launch the packaged app and confirm the template gallery has content.
2. Open a small project, edit, save and compile.
3. Check error navigation, SyncTeX, citation completion and PDF export.
4. Change the file outside the app; confirm the disk-change prompt and
   refusal to overwrite silently.
5. Check repository status on a Git project and one non-Git project.
6. Restart the app after a dirty edit and confirm recovery is offered.

## Current verification

Refreshed after the M7.1–M7.3 harness work (61 engine commands / 61 MCP
tools).

- **Windows x64 — passed locally.** Artifact:
  `dist/warraqcraft-0.1.0-windows-x64.zip`
  SHA-256: `220201edce45615f9d157aadf850ba971bd857f3c1f69c16b0f7844616eadca6`
- Package inspection found both executables, bundled `bin/tectonic.exe`,
  all 144 shipped templates, README, licences and NOTICE.
- `dist/warraqcraft-0.1.0-windows-x64-setup.exe` — per-user NSIS
  installer — was built locally, silent-installed (`/S`), verified to
  compile a document via the installed `bin\tectonic.exe` (a real
  one-page PDF), and uninstalled leaving no files, registry entries or
  Start Menu items behind.
- The packaged CLI listed 61 engine commands, returned all 144 templates
  and produced project structure; MCP `initialize`/`tools/list` returned
  61 tools.
- The packaged app launched with a control file and served `source.get`,
  `document.disk_state` and `ai.components` (4 built-in skills, 6 eval
  fixtures visible). Bundled Tectonic compiled a real document to a
  one-page `.warraqcraft/main.pdf`.
- External disk modification made `document.disk_state` report
  `external_changed: true`; a normal save was refused and
  `document.save {"overwrite":true}` succeeded.
- **Linux x86_64 — passed in Ubuntu 22.04 WSL2/WSLg.** Artifacts:
  `dist/warraqcraft-0.1.0-linux-x86_64.tar.gz`
  SHA-256: `a70b369505d51995c275b2395ca10fee9ee286c9bf76722b97133c3196c4f129`;
  `dist/warraqcraft-0.1.0-linux-x86_64.deb`
  SHA-256: `776bda84aab315ab89575d601d0a8c803d9df5954142f890a8af181b679e32ab`.
- The `.deb` installed with `dpkg -i`; `/usr/bin/warraqcraft-cli` listed
  61 commands and found all 144 templates under
  `/usr/share/warraqcraft/templates`; files install `root:root` `755`.
- The packaged GUI launched through WSLg and served `source.get` and
  `ai.eval.list`; MCP `initialize`/`tools/list` returned 61 tools.
- The tarball's bundled `bin/tectonic` (musl 0.17.0) compiled a real
  document to a one-page `/tmp/wc-linux-smoke/.warraqcraft/main.pdf`.
- Linux packaging gained one more fix: `dpkg-deb` refuses a `DEBIAN`
  directory above `0775`, and drvfs/WSL-mounted checkouts report every
  file `777` while ignoring `chmod`. The script now stages the package
  root under `mktemp -d` on a native filesystem and normalizes
  permissions before building the `.deb`.
- **macOS — covered by release-CI runtime smoke.** The release workflow
  now exercises every packaged artifact on its own OS: the packaged CLI
  runs `commands`, `template.list`, `project.structure` and an MCP
  `initialize`/`tools/list` handshake, and on `macos-latest` the built
  `WarraqCraft.app` is launched with a control file and must answer
  `source.get` and `document.disk_state` through the packaged CLI. A
  real-desktop interactive macOS pass (Gatekeeper UX, rendering,
  shortcuts) remains the one open manual item.

An earlier exploratory Windows packaged run exited before a follow-up
control call. A clean rerun stayed alive through the same compile path,
so this was not reproduced; keep watching for it during platform QA.
