# Release QA

This checklist is the release-artifact gate for WarraqCraft. Run it on the
same OS as the artifact whenever possible; CI packaging checks do not
replace real desktop smoke testing.

## Artifact expectations

| Platform | Artifact | Required contents | Runtime notes |
| --- | --- | --- | --- |
| Windows | `dist/warraqcraft-<version>-windows-<arch>.zip` | `warraqcraft.exe`, `warraqcraft-cli.exe`, `templates/`, README, licences, NOTICE; `bin/tectonic.exe` when packed with `-IncludeTectonic` | Portable zip; unsigned binaries can trigger SmartScreen. |
| macOS | `dist/warraqcraft-<version>-macos-<arch>.tar.gz`, `.dmg` | tarball plus `WarraqCraft.app`; app resources must include `templates/`, README, licences and NOTICE | Unsigned/unnotarized builds can be blocked by Gatekeeper. |
| Linux | `dist/warraqcraft-<version>-linux-<arch>.tar.gz`, `.deb` | tarball; deb installs binaries to `/usr/bin`, templates to `/usr/share/warraqcraft/templates`, docs to `/usr/share/doc/warraqcraft`, and a desktop entry | `.deb` is an installable package, not a repository package; TeX/Tectonic remains external unless bundled separately. |

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

- **Windows x64 — passed locally.** Artifact:
  `dist/warraqcraft-0.1.0-windows-x64.zip`
  SHA-256: `2E6AA1E0B90735E64A9A6AAB40657E0596D06552B57EE05AA097BF2503B2E95D`
- Package inspection found both executables, bundled `bin/tectonic.exe`,
  all 20 shipped templates, README, licences and NOTICE.
- The packaged CLI listed the engine commands and found all templates.
- The packaged app launched with a control file, served `source.get`,
  `document.disk_state`, `document.save`, `document.compile` and
  `repo.status`, and stayed alive through the compile.
- Bundled Tectonic produced `target/release-smoke/.warraqcraft/main.pdf`
  with one page.
- External disk modification made `document.disk_state` report
  `external_changed: true`; a normal save was refused and
  `document.save {"overwrite":true}` succeeded.
- MCP `initialize` and `tools/list` returned the expected protocol and
  tool catalog.
- `scripts/package.sh` passed POSIX shell syntax checking;
  `scripts/package.ps1` passed PowerShell parsing and produced the tested
  zip. The three-OS CI/release workflow definitions were inspected.
- macOS and Linux artifacts still require execution on real macOS/Linux
  hosts or CI runners; package layout discovery was updated so `.app`
  `Contents/Resources/templates` and `/usr/share/warraqcraft/templates`
  are found.

An earlier exploratory packaged run exited before a follow-up control
call. A clean rerun stayed alive through the same compile path, so this
was not reproduced; keep watching for it during platform QA.
