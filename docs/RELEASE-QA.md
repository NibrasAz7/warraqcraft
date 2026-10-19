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
- **Linux x86_64 — passed in Ubuntu 22.04 WSL2/WSLg.** Artifacts:
  `warraqcraft-0.1.0-linux-x86_64.tar.gz`
  SHA-256: `f2b664a829cf4c90b69a6ee616276fea2f7ff28e5688287a2b28eedcd9763178`;
  `warraqcraft-0.1.0-linux-x86_64.deb`
  SHA-256: `4e07ad09a07b39556665423a9531a003df0b5bf8511a85dbcfb99cf55b2daace`.
- The tarball contained both executables, all 20 templates, README,
  licences and NOTICE. The packaged CLI listed commands, returned all 20
  templates, produced project structure, and completed MCP
  `initialize`/`tools/list` with 37 tools.
- The packaged GUI launched through WSLg, served control-channel
  `source.get`, `document.disk_state` and `document.compile`, and stayed
  alive through compile. The Linux musl Tectonic binary produced
  `/root/wc-linux-smoke/.warraqcraft/main.pdf` with one page.
- External disk modification made `document.disk_state` report
  `external_changed: true`; a normal save was refused and
  `document.save {"overwrite":true}` succeeded.
- The `.deb` installed with `dpkg -i`; `/usr/bin/warraqcraft-cli` found
  all 20 templates from `/usr/share/warraqcraft/templates`, the desktop
  entry installed, and `/usr/bin/warraqcraft` served the control channel.
- Linux packaging found two issues that are fixed here:
  `scripts/package.sh` needs an executable bit for a POSIX checkout, and
  the Tectonic fetcher needs `x86_64-unknown-linux-musl` because the
  glibc build requires a newer glibc than Ubuntu 22.04.
- macOS artifacts still require execution on a real macOS host or CI
  runner; `.app` `Contents/Resources/templates` discovery is covered by
  the added path check but remains runtime-unverified.

An earlier exploratory Windows packaged run exited before a follow-up
control call. A clean rerun stayed alive through the same compile path,
so this was not reproduced; keep watching for it during platform QA.
